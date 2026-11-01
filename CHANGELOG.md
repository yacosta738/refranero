# Changelog

## [0.3.0](https://github.com/yacosta738/refranero/compare/v0.2.0...v0.3.0) (2026-11-01)


### Features

* **release:** publish to crates.io, npm, Docker Hub and GHCR on release ([#12](https://github.com/yacosta738/refranero/issues/12)) ([fe1a225](https://github.com/yacosta738/refranero/commit/fe1a22588b324865a2cc5462a2e388217793cc3e))


### Chores

* **deps:** update docker/build-push-action action to v6.19.2 ([#14](https://github.com/yacosta738/refranero/issues/14)) ([ce88dae](https://github.com/yacosta738/refranero/commit/ce88dae5a5d10d61dc7223c55e942acc45a2cfb5))
* **deps:** update docker/build-push-action action to v7 ([#18](https://github.com/yacosta738/refranero/issues/18)) ([e047770](https://github.com/yacosta738/refranero/commit/e047770dcfbc3e779c08c42220f73a342a0a4e52))
* **deps:** update docker/login-action action to v3.7.0 ([#15](https://github.com/yacosta738/refranero/issues/15)) ([434540d](https://github.com/yacosta738/refranero/commit/434540d18533a1b2851440843cfa0f94267ec043))
* **deps:** update docker/login-action action to v4 ([#19](https://github.com/yacosta738/refranero/issues/19)) ([490e260](https://github.com/yacosta738/refranero/commit/490e2600700d9aa35784a5211b2008cf607ef664))
* **deps:** update docker/setup-buildx-action action to v3.12.0 ([#16](https://github.com/yacosta738/refranero/issues/16)) ([dbfbd5f](https://github.com/yacosta738/refranero/commit/dbfbd5f57ceb41be19cb612e3be3147a3e3b4513))
* **deps:** update docker/setup-qemu-action action to v3.7.0 ([#17](https://github.com/yacosta738/refranero/issues/17)) ([ed4a25c](https://github.com/yacosta738/refranero/commit/ed4a25c55630d254c3463295ca975bf1bd102902))

## [0.2.0](https://github.com/yacosta738/refranero/compare/v0.1.0...v0.2.0) (2026-11-01)


### Features

* serve cache while refreshing in background ([4854875](https://github.com/yacosta738/refranero/commit/4854875a0f53318656c2116624066e5e17bc7514))
* update .gitignore to exclude CodeGraph data files ([06bbdff](https://github.com/yacosta738/refranero/commit/06bbdff51065d2dc343248b03a6b06df249ffdcc))


### Bug Fixes

* **ci:** align native package versions with wrapper ([263c6f5](https://github.com/yacosta738/refranero/commit/263c6f53dd7642e479059d393469bb5b2230d09d))
* **ci:** specify stable rust toolchain ([5558b9d](https://github.com/yacosta738/refranero/commit/5558b9d0bcaf5066258eedbf868c16f21b929531))
* **deps:** update rust crate dirs to v7 ([#7](https://github.com/yacosta738/refranero/issues/7)) ([ea313d2](https://github.com/yacosta738/refranero/commit/ea313d2221ab8eab7225373edaf822de427ec9a9))
* **test:** use current platform package in wrapper test ([183c60a](https://github.com/yacosta738/refranero/commit/183c60ac8866efb6081f9bed11ff0ad1a15c28e6))


### Documentation

* **ci:** note GITHUB_TOKEN PR-creation permission requirement ([#10](https://github.com/yacosta738/refranero/issues/10)) ([51567d4](https://github.com/yacosta738/refranero/commit/51567d468135fe2f5801f6a450101deaf0536a4c))
* record CI fixes and pull request state ([8fd1eb3](https://github.com/yacosta738/refranero/commit/8fd1eb39926416c7df9f889eb1869f3e866895bb))


### Continuous Integration

* pin actions and add release please ([0faede9](https://github.com/yacosta738/refranero/commit/0faede990662108b65063afc79469a17b9ac716d))
* pin actions and add release please ([e6629f8](https://github.com/yacosta738/refranero/commit/e6629f87153092d342f54e3df8dc3d244fcdd2a5))
* **release:** authenticate Release Please with GitHub App client id ([b08c066](https://github.com/yacosta738/refranero/commit/b08c066b1d1f4a2b3453fd7b847eff56f6aee195))
* **release:** grant issues write for release labeling ([d85dba8](https://github.com/yacosta738/refranero/commit/d85dba883f89d91e428834e7d62bf54c3c28bbf9))
* **release:** use GitHub App client id and add manual trigger ([1546b4f](https://github.com/yacosta738/refranero/commit/1546b4ff4c47439891c566fd9d4940c01f1a9656))
* **release:** use GITHUB_TOKEN for Release Please instead of GitHub App ([#9](https://github.com/yacosta738/refranero/issues/9)) ([ddc9ba7](https://github.com/yacosta738/refranero/commit/ddc9ba753f76137c51c84cb3eee3081169bc8ad4))
* use app token for release please ([801a948](https://github.com/yacosta738/refranero/commit/801a948620ed787c487f793421fe797aee8a1c0c))


### Chores

* **ci:** extend diagnostic to list installation repositories ([06bae82](https://github.com/yacosta738/refranero/commit/06bae82da6881652ada9442a383990eb587b8eb7))
* **ci:** temporary diagnostic workflow for app installation scope ([#8](https://github.com/yacosta738/refranero/issues/8)) ([21f8679](https://github.com/yacosta738/refranero/commit/21f86799180369a0d3c42e2b9a274109537f902b))
* **deps:** configure Renovate update policy ([da896ab](https://github.com/yacosta738/refranero/commit/da896ab331fd1cfe43b0134ea7120e3920bb1a12))
* **deps:** update dependency node to v26 ([28d20d9](https://github.com/yacosta738/refranero/commit/28d20d9fdbe69e1fd096cf1434d11d3ca344d920))
* **deps:** update dependency node to v26 ([ba36fe6](https://github.com/yacosta738/refranero/commit/ba36fe641818dd1b873dc4d7f3e11163ed0b0ff5))
* **release:** add local GitHub App secret setup helper ([fbdd13d](https://github.com/yacosta738/refranero/commit/fbdd13dde5b506e50ecc68efe6767824ece7e261))
