## [0.7.1](https://github.com/foro-sh/claudius-maximus/compare/v0.7.0...v0.7.1) (2026-09-26)

### Bug Fixes

* **git:** fail the push when origin refuses the ref ([093d7f2](https://github.com/foro-sh/claudius-maximus/commit/093d7f220adcb2630ede99d30a102cb14fd5e76a))

## [0.7.0](https://github.com/foro-sh/claudius-maximus/compare/v0.6.0...v0.7.0) (2026-09-22)

### Features

* **add-instance:** assign Latin-ordinal instance names ([9dc96d6](https://github.com/foro-sh/claudius-maximus/commit/9dc96d61b7ae58a0dc1667d50671c6e1ec6d9d25))
* **deploy:** hand every instance its own status port ([4028c3b](https://github.com/foro-sh/claudius-maximus/commit/4028c3bbb00ae67d5f7d8210ccf49eae5a9b3e7d))
* **deploy:** run the unit as a notify service with a watchdog ([38ca60f](https://github.com/foro-sh/claudius-maximus/commit/38ca60f7c98d9a6097bc4f19fd40cab6bfa5fedd))
* **github:** carry issue bodies and read issue comments back ([3e0f8df](https://github.com/foro-sh/claudius-maximus/commit/3e0f8dfe916ed1f4869507497760a2c0fb6a49d0))
* **worker:** clone a repo that is not on the box yet ([57f86dc](https://github.com/foro-sh/claudius-maximus/commit/57f86dccc42e47c04e78982f0c0aac001cf9f6c1))
* **worker:** hand Claude the issue and the plan it wrote ([71f06ca](https://github.com/foro-sh/claudius-maximus/commit/71f06ca47ad03faf93b4c7427036e93052ab4117))
* **worker:** say what it is doing while it is doing it ([919438a](https://github.com/foro-sh/claudius-maximus/commit/919438ac0f70e836b62aa0920864ebdcedbfbe5c))
* **worker:** serve the register over http ([1c2f270](https://github.com/foro-sh/claudius-maximus/commit/1c2f2705709b1339a48cd5be55f4a2d10c0eb9c9))
* **worker:** survive real deployments ([#13](https://github.com/foro-sh/claudius-maximus/issues/13)) ([ad3bde7](https://github.com/foro-sh/claudius-maximus/commit/ad3bde703e3d2f5518fb71f125246f89b7ee8722))

### Bug Fixes

* **add-instance:** nest default clones under owner/name ([d4369e1](https://github.com/foro-sh/claudius-maximus/commit/d4369e1c4848d20446839be9242f174cbd2282c5))
* **deploy:** compare status addresses by port, not by spelling ([c2b11a6](https://github.com/foro-sh/claudius-maximus/commit/c2b11a6b90d1b29bc1208eca6dae4dc07067d0a2))
* **deploy:** stop offering a status page that is turned off ([364e575](https://github.com/foro-sh/claudius-maximus/commit/364e57540ea88e706e582e085cdf755bdd42e29a))
* **git:** authenticate the fetch in sync_branch ([3755f6e](https://github.com/foro-sh/claudius-maximus/commit/3755f6ebcda9fd9c4bb6259c5e26ce1568b78b8c))
* **github:** keep the OAuth token in a file, not the OS keyring ([8067a4f](https://github.com/foro-sh/claudius-maximus/commit/8067a4f0b340f672e9db6f96bae81726d779ba0c))
* **worker:** back off an issue that keeps failing ([19e5f53](https://github.com/foro-sh/claudius-maximus/commit/19e5f530a0f9fc27cef3333f2ef54f56cd33be33))
* **worker:** correct what claudius_healthy says it measures ([10d714d](https://github.com/foro-sh/claudius-maximus/commit/10d714d9e2732ba3d32ca358ece0c591e497f6ff))
* **worker:** decide what an issue needs before touching its clone ([fd393d9](https://github.com/foro-sh/claudius-maximus/commit/fd393d978dde5c29298443427026728797d44a81))
* **worker:** drain claude's output while the prompt is still going in ([3bef3d3](https://github.com/foro-sh/claudius-maximus/commit/3bef3d35ecad22f7ea28c7c9a806630ccc10baba))
* **worker:** escape repo names one at a time, and pause on accept errors ([fef4d1c](https://github.com/foro-sh/claudius-maximus/commit/fef4d1c4dca424113f707c784a35eceed4bacdce))
* **worker:** feed the prompt on stdin and refuse an empty plan ([6decdee](https://github.com/foro-sh/claudius-maximus/commit/6decdee16e68e458102a5c7e93ca168de5785436))
* **worker:** judge health by what moved last, not by the last sweep ([d551bf1](https://github.com/foro-sh/claudius-maximus/commit/d551bf12e755bbf62a67e06efd3d111143063ad8))
* **worker:** keep systemd's variables out of the child, not the process ([d9bf16e](https://github.com/foro-sh/claudius-maximus/commit/d9bf16e9d51aa32e44183ebdd54d640bc9a48e2b))
* **worker:** keep the pulse under any notify unit, not only a watched one ([35face7](https://github.com/foro-sh/claudius-maximus/commit/35face7230ff1ba060df604af8596d9e18782bc0))
* **worker:** key a repeated warning on what failed, not on why ([2875b12](https://github.com/foro-sh/claudius-maximus/commit/2875b120b680bb7bd03a8c332d06a829c589ec8a))
* **worker:** key repeated warnings on the error, not just the issue ([415c62c](https://github.com/foro-sh/claudius-maximus/commit/415c62cafac06f8a7dc72411d8c3e032225dbb6a))
* **worker:** let a window notice Mattermost missed be sent again ([3dbdc74](https://github.com/foro-sh/claudius-maximus/commit/3dbdc7477642725e961dfa12865533a9d4b2517b))
* **worker:** make the plan comment editable and the failures visible ([d6747e5](https://github.com/foro-sh/claudius-maximus/commit/d6747e540634217c9ef573706ae28f500e152689))
* **worker:** move the register issue by issue, not repo by repo ([1edc985](https://github.com/foro-sh/claudius-maximus/commit/1edc98502d29eb791426d8e5e5f1ffa66451de5f))
* **worker:** notify a stuck repo once, a stuck issue once each ([6e9daa1](https://github.com/foro-sh/claudius-maximus/commit/6e9daa1aebf6d050d7a3219addee13f73525e626))
* **worker:** only follow a plan comment the worker itself wrote ([2f3cfc0](https://github.com/foro-sh/claudius-maximus/commit/2f3cfc01ac6fb72d08d836a8baa9ff0bdd54ce41))
* **worker:** open the PR against origin's default branch ([0055754](https://github.com/foro-sh/claudius-maximus/commit/0055754a1753ce5e005d05980373ba1d5be6bfe3))
* **worker:** read the usage window off stderr only ([7063c92](https://github.com/foro-sh/claudius-maximus/commit/7063c92ba90992069bdb4b8cd185659dec48a3d2))
* **worker:** say a repeating failure once, not once a minute ([5492b90](https://github.com/foro-sh/claudius-maximus/commit/5492b9039ae1c7d92918eef8837679c66f180492))
* **worker:** stop a stage or a spent window from latching ([d6aec0c](https://github.com/foro-sh/claudius-maximus/commit/d6aec0cf1b90b4bb5ac79a3f764bbec2a592df5b))
* **worker:** stop shipping a branch claude never committed to ([dd868cf](https://github.com/foro-sh/claudius-maximus/commit/dd868cfc6ec0accdd0b6947d6127b9f79a602630))
* **worker:** sync the clone before every issue, not once per sweep ([592b70e](https://github.com/foro-sh/claudius-maximus/commit/592b70e10095e3fdfed5e17aee3f5d57f080fd2a))

## [0.6.0](https://github.com/foro-sh/claudius-maximus/compare/v0.5.0...v0.6.0) (2026-09-16)

### Features

* **github:** open pull requests ([eaff807](https://github.com/foro-sh/claudius-maximus/commit/eaff807a5e5b7fc59db4cabfa84a075941ca5698))
* **worker:** push the branch and open the PR itself ([cbea003](https://github.com/foro-sh/claudius-maximus/commit/cbea003248d1f7b89b24a2bb43c6568bbc634fda))
* **worker:** run against the real GitHub client ([1423ee5](https://github.com/foro-sh/claudius-maximus/commit/1423ee5d4c91c5b54efad94ae387577e1da92488))
* **worker:** spawn agents against real GitHub ([#9](https://github.com/foro-sh/claudius-maximus/issues/9)) ([6c01ede](https://github.com/foro-sh/claudius-maximus/commit/6c01ede56f048b68d60afb931c6b277daaecb293))

## [0.5.0](https://github.com/foro-sh/claudius-maximus/compare/v0.4.0...v0.5.0) (2026-09-16)

### Features

* provision an instance with add-instance.sh ([ead19a4](https://github.com/foro-sh/claudius-maximus/commit/ead19a457a230631b5fb5c4b6f5bc9f77624e9cc))

### Bug Fixes

* harden add-instance.sh against three sharp edges ([f429be9](https://github.com/foro-sh/claudius-maximus/commit/f429be9508786395f6718179088986195219908a))

## [0.4.0](https://github.com/foro-sh/claudius-maximus/compare/v0.3.0...v0.4.0) (2026-09-15)

### Features

* **github:** implement GithubClient on octocrab with device-flow auth ([13a8930](https://github.com/foro-sh/claudius-maximus/commit/13a893012b0a09f8acdece938befd500976a3e6c))

### Bug Fixes

* **github:** fail instead of re-authorizing a rejected stored token ([9a4e48d](https://github.com/foro-sh/claudius-maximus/commit/9a4e48dbd7a28620465bea1bd3f23a20b671ab2e))

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
