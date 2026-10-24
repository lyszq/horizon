# iOS bundle layout

`tools/package_ios.py` creates the app bundle and archive directly from the Rust executable and the project's
original PNG icon. The generated `Info.plist` declares `dev.lyszq.horizon`, iPad support, Metal, and minimum iOS
26.0. The device IPA contains no signature or provisioning profile; the simulator app receives a local ad-hoc
signature so `simctl` can install it. This folder is reserved for future iOS-specific bundle resources; game data
must remain player-supplied and outside the package.

The public build and smoke workflow is documented in [docs/ios.md](../../docs/ios.md).
