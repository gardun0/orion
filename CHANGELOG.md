# Changelog

## [0.1.5](https://github.com/gardun0/orion/compare/v0.1.4...v0.1.5) (2026-08-13)


### Features

* per-application audio capture on Windows (WASAPI loopback) and macOS (Core Audio taps) ([4dba3bb](https://github.com/gardun0/orion/commit/4dba3bb0bc06e63fbdf09b9818cd2cce4a500025))


### Documentation

* showcase mixer screenshot and deduplicate feature-table icons in README ([7780c37](https://github.com/gardun0/orion/commit/7780c370ecd6b62d9e3402c84f9b62b685786c41))

## [0.1.4](https://github.com/gardun0/orion/compare/v0.1.3...v0.1.4) (2026-08-11)


### Bug Fixes

* pin windows/windows-core to 0.62.2 for a consistent WASAPI build ([f579eab](https://github.com/gardun0/orion/commit/f579eab838e5ecafc0091590a8541d65c6625a4b))

## [0.1.3](https://github.com/gardun0/orion/compare/v0.1.2...v0.1.3) (2026-08-11)


### Features

* bus clip protection, mode crossfade, RMS/clip meters, cpal backend ([b51e345](https://github.com/gardun0/orion/commit/b51e345d0b1b5fed8fc1c281909587b952d03e06))


### Bug Fixes

* build release packages on ubuntu-24.04 and allow manual re-packaging ([8b31dd6](https://github.com/gardun0/orion/commit/8b31dd668153139fb3ec23ff7fc57e383aa1fce5))
* build release packages on ubuntu-24.04 and allow manual re-packaging ([ee72958](https://github.com/gardun0/orion/commit/ee7295889c1705be431736bcbb11dc741bd30053))
* mark pre-1.0 releases as prerelease on manual dispatch runs too ([cc155e6](https://github.com/gardun0/orion/commit/cc155e61131832e2a870e0f6cd144c423f1cbdf3))
* repair Windows and macOS CI builds ([b9f2e1f](https://github.com/gardun0/orion/commit/b9f2e1fe21f805e55adfe6f7d47a8b3809581b3e))

## [0.1.2](https://github.com/gardun0/orion/compare/v0.1.1...v0.1.2) (2026-08-11)


### Features

* PipeWire mixer with destination-driven block engine ([c66c3f9](https://github.com/gardun0/orion/commit/c66c3f94be7afa2ea22f49bfd2b29d267c17111a))


### Bug Fixes

* use Option::zip for route cell lookups ([1b7fdb0](https://github.com/gardun0/orion/commit/1b7fdb0a01281aef0a2b7a280692b04a3cc9d4a4))


### Documentation

* add color-baked README feature icons ([bab1047](https://github.com/gardun0/orion/commit/bab104739d2f625d7299ecbaa60e5d1de59e25cf))


### Miscellaneous Chores

* **main:** release 0.1.1 ([#1](https://github.com/gardun0/orion/issues/1)) ([0339a1e](https://github.com/gardun0/orion/commit/0339a1e7879787a017759886dd0e775049aa1a3f))
* project scaffold and inital official commit ([ba81c00](https://github.com/gardun0/orion/commit/ba81c002fb91cb5aa86ab8c0492d29781e40c3aa))

## [0.1.1](https://github.com/gardun0/orion/compare/v0.1.0...v0.1.1) (2026-08-11)


### Features

* PipeWire mixer with destination-driven block engine ([c66c3f9](https://github.com/gardun0/orion/commit/c66c3f94be7afa2ea22f49bfd2b29d267c17111a))


### Bug Fixes

* use Option::zip for route cell lookups ([1b7fdb0](https://github.com/gardun0/orion/commit/1b7fdb0a01281aef0a2b7a280692b04a3cc9d4a4))


### Documentation

* add color-baked README feature icons ([bab1047](https://github.com/gardun0/orion/commit/bab104739d2f625d7299ecbaa60e5d1de59e25cf))


### Miscellaneous Chores

* project scaffold and inital official commit ([ba81c00](https://github.com/gardun0/orion/commit/ba81c002fb91cb5aa86ab8c0492d29781e40c3aa))
