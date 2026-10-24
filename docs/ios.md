# iOS and iPad package

The public iOS workflow builds the existing `horizon-client` Rust executable for physical arm64 iOS and the Apple
Silicon simulator. It uses the `iphoneos` and `iphonesimulator` SDKs selected by Xcode on the standard
`macos-26` GitHub-hosted runner, sets the minimum OS to iOS 26.0, and records the exact Rust, Xcode, SDK, and commit
versions in each package's `build-metadata.json`. Until the first green run establishes a verified baseline, the
workflow follows installed Rust `stable` and the runner's selected stable Xcode; the recorded versions are the
inputs for pinning that baseline.

The app bundle identifier is `dev.lyszq.horizon`. The package builder takes only Cargo's `horizon-client` Mach-O
and the original Horizon icon under `packaging/icons/`; it never reads `.local/`, Minecraft assets, saves, or world
data. The device `.ipa` is unsigned and has no embedded provisioning profile, so the recipient can sign it with
their own Apple team and profile. The simulator `.app.zip` is ad-hoc signed for `simctl` installation. Each package
includes build metadata, and each artifact has a SHA-256 sidecar. Release line tables are retained and exported as
separate dSYM archives.

The simulator job launches on an available iOS 26 iPad simulator with `HORIZON_IOS_SMOKE=1`. A successful report
at `Documents/horizon-ios-smoke.json` must contain schema 1, `status: "pass"`, `scope: "bootstrap-metal"`, at least
three presented frames, and at least one app tick. CI also saves the report, simulator log, and screenshot. This
proves the app bootstrap, winit lifecycle, and Metal surface clear/present path on Apple's simulator. It does not
prove gameplay, world/resource loading, touch controls, sustained frame rate, or behavior on a physical iPad.

## Physical iPad acceptance

Before describing the client as playable or performant on iPad, install a re-signed build on a real supported iPad
and test it with the resources supplied by the player. Record the device model, iPadOS version, package commit,
signing configuration, and whether the test used fresh app data. Keep the captured report, screen recording, and
measurements with the build hash.

Run a continuous 30-minute session after a cold launch. Exercise world entry, movement, camera rotation, touch
controls, menus, pause/resume, rotation, and return to the app after backgrounding. Record frame time and tick time
at one-second intervals, including median, p95, p99, and missed-frame counts. Record resident memory at launch,
after world entry, and every five minutes, along with iPadOS thermal state and any OS memory-pressure or termination
events. Note any visual corruption, input loss, crashes, and whether performance changes as the device heats up.

Keep simulator smoke, physical-device acceptance, and gameplay/resource testing as separate evidence. A green
workflow run is only the bootstrap Metal smoke described above.
