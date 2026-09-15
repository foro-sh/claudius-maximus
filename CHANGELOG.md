## [0.3.0](https://github.com/foro-sh/claudius-maximus/compare/v0.2.0...v0.3.0) (2026-09-15)

### Features

* **worker:** claim one instance per label with an OS file lock ([968a43b](https://github.com/foro-sh/claudius-maximus/commit/968a43b3edae1a4d0c0afc03a474fb1bde1a287b))
* **worker:** drive git through the real Git2Ops ([86a9328](https://github.com/foro-sh/claudius-maximus/commit/86a932869fc9bb4e62be2ade73d55e8603ef2c13))
* **worker:** port the plan-then-implement state machine and poll loop ([d5ff391](https://github.com/foro-sh/claudius-maximus/commit/d5ff391eb2cb7da8502c78c2eb0028b0921bb4b4))
* **worker:** post status lines to Mattermost ([ce93548](https://github.com/foro-sh/claudius-maximus/commit/ce93548b5d182d18a6af681620e762516467c21c))

### Bug Fixes

* **worker:** keep sweeping a repo whose clone fails to sync ([8dc5ae5](https://github.com/foro-sh/claudius-maximus/commit/8dc5ae53a17d5ff6fa2c693f487f8aabd89b33f9))

## [0.2.0](https://github.com/foro-sh/claudius-maximus/compare/v0.1.0...v0.2.0) (2026-09-15)

### Features

* **git:** implement GitOps on vendored libgit2 ([d0304d8](https://github.com/foro-sh/claudius-maximus/commit/d0304d8678109c236aafcd9a9cb937fcd9eb7003)), closes [foro-sh/claudius-maximus#1](https://github.com/foro-sh/claudius-maximus/issues/1)

### Bug Fixes

* **git:** clear untracked files when syncing a branch ([7f985d5](https://github.com/foro-sh/claudius-maximus/commit/7f985d58268e4cf7affc6a6d9bed680ea73d7fb3)), closes [foro-sh/claudius-maximus#1](https://github.com/foro-sh/claudius-maximus/issues/1)

## [0.1.0](https://github.com/foro-sh/claudius-maximus/compare/v0.0.0...v0.1.0) (2026-09-15)

### Features

* scaffold Cargo workspace with GithubClient/GitOps trait contracts ([a823835](https://github.com/foro-sh/claudius-maximus/commit/a823835abd9bcaea262e7de9ccec6751876e6a13)), closes [#1](https://github.com/foro-sh/claudius-maximus/issues/1)
