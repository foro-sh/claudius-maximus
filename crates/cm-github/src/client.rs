//! The real [`GithubClient`], talking to GitHub's REST API through `octocrab`
//! and authenticated by OAuth device flow (foro-sh/claudius-maximus#1).
use anyhow::Context;
use async_trait::async_trait;
use http::header::ACCEPT;
use octocrab::Octocrab;
use octocrab::params::State;
use secrecy::{ExposeSecret, SecretString};

use crate::token_store::{KeyringStore, TokenStore};
use crate::{GithubClient, Issue};

/// Where the API lives. Device flow talks to the website, not the API host.
const GITHUB_API: &str = "https://api.github.com";
const GITHUB_WEB: &str = "https://github.com";

/// `repo` scope covers everything the worker does: read issues, write labels
/// and comments, and push over HTTPS with the same token.
const SCOPES: [&str; 1] = ["repo"];

pub struct OctocrabGithubClient {
    crab: Octocrab,
    token: String,
}

impl OctocrabGithubClient {
    /// Reuse the token stored for `instance_name`, or run the device flow and
    /// store the one it yields.
    ///
    /// `client_id` is the id of a GitHub OAuth App with device flow enabled —
    /// a deployment detail, so it is passed in rather than baked in here.
    ///
    /// The device flow blocks: it prints a verification URL and a user code,
    /// then polls GitHub until the operator authorizes. That is the same
    /// one-time ceremony `gh auth login` asks for, and it only happens when
    /// the keychain holds no usable token.
    pub async fn login_or_load(instance_name: &str, client_id: &str) -> anyhow::Result<Self> {
        Self::login_or_load_with(
            instance_name,
            client_id,
            &KeyringStore,
            GITHUB_API,
            GITHUB_WEB,
        )
        .await
    }

    pub(crate) async fn login_or_load_with(
        instance_name: &str,
        client_id: &str,
        store: &dyn TokenStore,
        api_uri: &str,
        web_uri: &str,
    ) -> anyhow::Result<Self> {
        if let Some(token) = store.load(instance_name)?
            && let Some(client) = Self::authenticated(api_uri, token).await?
        {
            return Ok(client);
        }
        // Falling through means there was no token, or the stored one is
        // revoked, expired, or issued against another OAuth app.

        let token = device_flow(client_id, web_uri).await?;
        store.store(instance_name, &token)?;
        Self::authenticated(api_uri, token)
            .await?
            .context("GitHub rejected the token it just issued")
    }

    /// Build a client on `token` and confirm GitHub still accepts it.
    /// `Ok(None)` means the token is unauthorized; an outer `Err` means the
    /// check itself failed, which is not a reason to re-run the device flow.
    async fn authenticated(api_uri: &str, token: String) -> anyhow::Result<Option<Self>> {
        let crab = Octocrab::builder()
            .base_uri(api_uri)
            .context("invalid GitHub API base URI")?
            .user_access_token(token.clone())
            .build()
            .context("building the GitHub client")?;

        // `/user` is the cheapest call that fails iff the token is no good.
        match crab.current().user().await {
            Ok(_) => Ok(Some(Self { crab, token })),
            Err(octocrab::Error::GitHub { source, .. })
                if source.status_code == http::StatusCode::UNAUTHORIZED =>
            {
                Ok(None)
            }
            Err(e) => Err(e).context("checking the stored token against GitHub"),
        }
    }

    /// The OAuth token this client holds, for `cm-git`'s HTTPS push.
    pub fn token(&self) -> &str {
        &self.token
    }

    fn repo<'a>(&self, repo: &'a str) -> anyhow::Result<(&'a str, &'a str)> {
        repo.split_once('/')
            .filter(|(owner, name)| !owner.is_empty() && !name.is_empty())
            .with_context(|| format!("expected repo as \"owner/name\", got {repo:?}"))
    }
}

#[async_trait]
impl GithubClient for OctocrabGithubClient {
    async fn list_labeled_issues(&self, repo: &str, label: &str) -> anyhow::Result<Vec<Issue>> {
        let (owner, name) = self.repo(repo)?;
        let page = self
            .crab
            .issues(owner, name)
            .list()
            .labels(&[label.to_owned()])
            .state(State::Open)
            .per_page(100)
            .send()
            .await
            .with_context(|| format!("listing {repo} issues labeled {label}"))?;

        let issues = self.crab.all_pages(page).await?;
        Ok(issues
            .into_iter()
            // The issues endpoint returns pull requests too; the worker only
            // deals in issues.
            .filter(|i| i.pull_request.is_none())
            .map(|i| Issue {
                number: i.number,
                author: i.user.login,
            })
            .collect())
    }

    async fn issue_labels(&self, repo: &str, number: u64) -> anyhow::Result<Vec<String>> {
        let (owner, name) = self.repo(repo)?;
        let page = self
            .crab
            .issues(owner, name)
            .list_labels_for_issue(number)
            .per_page(100)
            .send()
            .await
            .with_context(|| format!("listing labels on {repo}#{number}"))?;

        Ok(self
            .crab
            .all_pages(page)
            .await?
            .into_iter()
            .map(|l| l.name)
            .collect())
    }

    async fn add_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()> {
        let (owner, name) = self.repo(repo)?;
        self.crab
            .issues(owner, name)
            .add_labels(number, &[label.to_owned()])
            .await
            .with_context(|| format!("adding label {label} to {repo}#{number}"))?;
        Ok(())
    }

    async fn remove_label(&self, repo: &str, number: u64, label: &str) -> anyhow::Result<()> {
        let (owner, name) = self.repo(repo)?;
        self.crab
            .issues(owner, name)
            .remove_label(number, label)
            .await
            .with_context(|| format!("removing label {label} from {repo}#{number}"))?;
        Ok(())
    }

    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<()> {
        let (owner, name) = self.repo(repo)?;
        self.crab
            .issues(owner, name)
            .create_comment(number, body)
            .await
            .with_context(|| format!("commenting on {repo}#{number}"))?;
        Ok(())
    }

    async fn blocked_by_open_issue(&self, repo: &str, number: u64) -> anyhow::Result<bool> {
        let (owner, name) = self.repo(repo)?;

        /// Issue dependencies are new enough that octocrab 0.54 has no typed
        /// surface for them, so this goes through octocrab's own HTTP client
        /// rather than pulling in a second HTTP crate. Only `state` matters.
        #[derive(serde::Deserialize)]
        struct Blocker {
            state: String,
        }

        // ponytail: one page. 100 blockers on a single issue is not a shape
        // this worker will ever meet; paginate if that stops being true.
        let route =
            format!("/repos/{owner}/{name}/issues/{number}/dependencies/blocked_by?per_page=100");
        let blockers: Vec<Blocker> = self
            .crab
            .get(route, None::<&()>)
            .await
            .with_context(|| format!("reading blocked_by dependencies of {repo}#{number}"))?;

        Ok(blockers.iter().any(|b| b.state == "open"))
    }
}

/// Run GitHub's OAuth device flow to completion and return the access token.
async fn device_flow(client_id: &str, web_uri: &str) -> anyhow::Result<String> {
    let client_id = SecretString::from(client_id.to_owned());

    // Device flow is served by the website, and only speaks JSON if asked.
    let login = Octocrab::builder()
        .base_uri(web_uri)
        .context("invalid GitHub base URI")?
        .add_header(ACCEPT, "application/json".to_owned())
        .build()
        .context("building the device-flow client")?;

    let codes = login
        .authenticate_as_device(&client_id, SCOPES)
        .await
        .context("requesting a device code")?;

    println!(
        "Open {} and enter code {} to authorize Claudius Maximus.",
        codes.verification_uri, codes.user_code
    );

    let auth = codes
        .poll_until_available(&login, &client_id)
        .await
        .context("waiting for device flow authorization")?;

    Ok(auth.access_token.expose_secret().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_store::test_support::MemoryStore;
    use serde_json::{Value, json};
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const INSTANCE: &str = "cm-test";
    const CLIENT_ID: &str = "Iv1.testclientid";

    fn author(login: &str) -> Value {
        let url = format!("https://api.github.com/users/{login}");
        json!({
            "login": login,
            "id": 1,
            "node_id": "MDQ6VXNlcjE=",
            "avatar_url": url,
            "gravatar_id": "",
            "url": url,
            "html_url": url,
            "followers_url": url,
            "following_url": url,
            "gists_url": url,
            "starred_url": url,
            "subscriptions_url": url,
            "organizations_url": url,
            "repos_url": url,
            "events_url": url,
            "received_events_url": url,
            "type": "User",
            "site_admin": false,
            "name": null,
            "patch_url": null,
        })
    }

    fn issue_json(number: u64, login: &str, is_pull_request: bool) -> Value {
        let url = format!("https://api.github.com/repos/foro-sh/platform/issues/{number}");
        let mut issue = json!({
            "id": number,
            "node_id": "I_1",
            "url": url,
            "repository_url": url,
            "labels_url": url,
            "comments_url": url,
            "events_url": url,
            "html_url": url,
            "number": number,
            "state": "open",
            "title": "something to do",
            "body": null,
            "user": author(login),
            "labels": [],
            "assignees": [],
            "locked": false,
            "comments": 0,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        });
        if is_pull_request {
            issue["pull_request"] =
                json!({ "url": url, "html_url": url, "diff_url": url, "patch_url": url });
        }
        issue
    }

    /// Answers `GET /user`, so a stored token reads as valid.
    async fn mock_valid_user(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer stored-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(author("claudius")))
            .mount(server)
            .await;
    }

    /// A client already holding `stored-token`, pointed at the mock server.
    async fn client(server: &MockServer) -> OctocrabGithubClient {
        mock_valid_user(server).await;
        let store = MemoryStore::with_token(INSTANCE, "stored-token");
        OctocrabGithubClient::login_or_load_with(
            INSTANCE,
            CLIENT_ID,
            &store,
            &server.uri(),
            &server.uri(),
        )
        .await
        .unwrap()
    }

    /// Mounts the two device-flow endpoints, handing back `fresh-token`.
    async fn mock_device_flow(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dev-code",
                "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                // Smallest interval tokio's timer accepts; the first poll
                // fires immediately either way.
                "interval": 1,
            })))
            .mount(server)
            .await;

        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh-token",
                "token_type": "bearer",
                "scope": "repo",
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn reuses_a_stored_token_without_a_device_flow() {
        // No device-flow mocks: reaching them would 404 and fail the test.
        let server = MockServer::start().await;
        assert_eq!(client(&server).await.token(), "stored-token");
    }

    #[tokio::test]
    async fn runs_the_device_flow_when_nothing_is_stored_and_persists_the_token() {
        let server = MockServer::start().await;
        mock_device_flow(&server).await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer fresh-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(author("claudius")))
            .mount(&server)
            .await;

        let store = MemoryStore::default();
        let client = OctocrabGithubClient::login_or_load_with(
            INSTANCE,
            CLIENT_ID,
            &store,
            &server.uri(),
            &server.uri(),
        )
        .await
        .unwrap();

        assert_eq!(client.token(), "fresh-token");
        assert_eq!(store.get(INSTANCE).as_deref(), Some("fresh-token"));
    }

    #[tokio::test]
    async fn re_runs_the_device_flow_when_the_stored_token_is_rejected() {
        let server = MockServer::start().await;
        mock_device_flow(&server).await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer stored-token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "message": "Bad credentials",
                "documentation_url": "https://docs.github.com/rest",
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .and(header("authorization", "Bearer fresh-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(author("claudius")))
            .mount(&server)
            .await;

        let store = MemoryStore::with_token(INSTANCE, "stored-token");
        let client = OctocrabGithubClient::login_or_load_with(
            INSTANCE,
            CLIENT_ID,
            &store,
            &server.uri(),
            &server.uri(),
        )
        .await
        .unwrap();

        assert_eq!(client.token(), "fresh-token");
        assert_eq!(store.get(INSTANCE).as_deref(), Some("fresh-token"));
    }

    #[tokio::test]
    async fn a_failing_token_check_does_not_trigger_a_device_flow() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({
                "message": "Server Error",
            })))
            .mount(&server)
            .await;

        let store = MemoryStore::with_token(INSTANCE, "stored-token");
        let error = OctocrabGithubClient::login_or_load_with(
            INSTANCE,
            CLIENT_ID,
            &store,
            &server.uri(),
            &server.uri(),
        )
        .await
        .map(|_| ())
        .unwrap_err();

        assert!(
            error.to_string().contains("checking the stored token"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn list_labeled_issues_returns_issues_and_skips_pull_requests() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        Mock::given(method("GET"))
            .and(path("/repos/foro-sh/platform/issues"))
            .and(query_param("labels", "claudius-maximus"))
            .and(query_param("state", "open"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                issue_json(12, "danielsteman", false),
                issue_json(13, "octocat", true),
            ])))
            .mount(&server)
            .await;

        let issues = client
            .list_labeled_issues("foro-sh/platform", "claudius-maximus")
            .await
            .unwrap();

        assert_eq!(
            issues,
            vec![Issue {
                number: 12,
                author: "danielsteman".to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn issue_labels_returns_label_names() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        Mock::given(method("GET"))
            .and(path("/repos/foro-sh/platform/issues/12/labels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 1, "node_id": "L_1", "url": "https://api.github.com/l/1",
                  "name": "claudius-maximus", "color": "ededed", "default": false },
                { "id": 2, "node_id": "L_2", "url": "https://api.github.com/l/2",
                  "name": "cm:planning", "color": "ededed", "default": false },
            ])))
            .mount(&server)
            .await;

        assert_eq!(
            client.issue_labels("foro-sh/platform", 12).await.unwrap(),
            vec!["claudius-maximus".to_owned(), "cm:planning".to_owned()]
        );
    }

    #[tokio::test]
    async fn add_label_posts_the_label() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        Mock::given(method("POST"))
            .and(path("/repos/foro-sh/platform/issues/12/labels"))
            .and(body_json(json!({ "labels": ["cm:planning"] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        client
            .add_label("foro-sh/platform", 12, "cm:planning")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn remove_label_deletes_the_label() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        Mock::given(method("DELETE"))
            // octocrab percent-encodes the label name into the route.
            .and(path(
                "/repos/foro-sh/platform/issues/12/labels/cm%3Aplanning",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        client
            .remove_label("foro-sh/platform", 12, "cm:planning")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn comment_posts_the_body() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        Mock::given(method("POST"))
            .and(path("/repos/foro-sh/platform/issues/12/comments"))
            .and(body_json(json!({ "body": "planning" })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "id": 1,
                "node_id": "IC_1",
                "url": "https://api.github.com/c/1",
                "html_url": "https://github.com/c/1",
                "issue_url": "https://api.github.com/i/12",
                "body": "planning",
                "user": author("claudius"),
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
            })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .comment("foro-sh/platform", 12, "planning")
            .await
            .unwrap();
    }

    async fn blocked_by(server: &MockServer, blockers: Value) {
        Mock::given(method("GET"))
            .and(path(
                "/repos/foro-sh/platform/issues/12/dependencies/blocked_by",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(blockers))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn blocked_by_open_issue_is_true_only_while_a_blocker_is_open() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        blocked_by(
            &server,
            json!([issue_json(7, "octocat", false), {
                "number": 8,
                "state": "open",
            }]),
        )
        .await;

        assert!(
            client
                .blocked_by_open_issue("foro-sh/platform", 12)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn blocked_by_open_issue_is_false_when_every_blocker_is_closed() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        let mut closed = issue_json(7, "octocat", false);
        closed["state"] = json!("closed");
        blocked_by(&server, json!([closed])).await;

        assert!(
            !client
                .blocked_by_open_issue("foro-sh/platform", 12)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn blocked_by_open_issue_is_false_with_no_blockers() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        blocked_by(&server, json!([])).await;

        assert!(
            !client
                .blocked_by_open_issue("foro-sh/platform", 12)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_repo_that_is_not_owner_slash_name_is_rejected() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        let error = client.issue_labels("platform", 12).await.unwrap_err();
        assert!(
            error.to_string().contains("owner/name"),
            "unexpected error: {error:#}"
        );
    }
}
