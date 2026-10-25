# iOS and iPad package

The port starts from upstream v0.2.48 (`7f995af677c6558e87b1ba4b035dfe12037dc9f2`). Development is on
`ios/ipad` in the public `lyszq/horizon` fork, reviewed in draft PR #1. The first physical acceptance device is
an iPad Air M2 on iPadOS 26.3. An unsigned IPA still needs the user's sideload signing before it can run there.

## Private resource transfer and controls

With Rust, Python and JDK 25 available on the PC, prepare a transfer from your own installed Java Edition 26.3:

```powershell
python tools/prepare_ipad_resources.py --minecraft-dir "$env:APPDATA/.minecraft" --server-jar "C:/path/to/server.jar" --java-home "C:/path/to/jdk-25"
```

An existing prepared tree can be packaged without running Java:

```powershell
python tools/prepare_ipad_resources.py --prepared .local/horizon-client/26.3
```

Both routes produce `.local/ipad/horizon-resources-26.3.zip` and print its SHA-256. Keep this ZIP private and
separate from the IPA. On iPad, choose **Import resources** and select the ZIP in Files, then **Play**. The
importer checks schema 7 / Minecraft 26.3, paths, expansion limits and each resource's size/hash before
replacing the previous installation. Interrupted replacements are recovered on the next launch.

**Touch controls** opens the layout editor; the layout and look sensitivity persist in `horizon-touch.json`.
Gameplay offers movement, look, jump, sneak, sprint, break/use, inventory, drop, hotbar and pause. Menu swipes
scroll and a stationary long press performs a secondary click. GameController pads and physical keyboard/mouse
feed the same game mappings; optional controller haptics and raw HID are not implemented on iPad.
Control positions and the layout editor use the window's current safe area. While a system keyboard is docked,
menu layout and hit testing use the remaining visible height; the full native game drawable remains unchanged.
A floating keyboard leaves menu layout in place and can be moved by the player. UIKit supplies this geometry
through its [keyboard layout guide](https://developer.apple.com/documentation/uikit/uikeyboardlayoutguide).

The in-game **Files** button first cancels input and saves/closes the current world. Only then can world import
or backup export run. Backups contain world ZIPs and available diagnostic reports; they do not contain the
Minecraft resources. After replacing resources from a running game, close and reopen the app before playing.
The setup screen's **Benchmark** runs the existing standard village/forest/ocean/mountain scenario with the
current graphics settings. Benchmark operation temporarily removes the frame cap; ordinary play defaults to
60 FPS, 12-chunk render/simulation distances and the native drawable size, without automatic quality reduction.

## Public Apple build

The public iOS workflow builds the existing `horizon-client` Rust executable for physical arm64 iOS and the Apple
Silicon simulator. It installs Rust 1.99.1 and both Apple Silicon iOS targets, uses the `iphoneos` and
`iphonesimulator` SDKs selected by Xcode on the standard `macos-26` GitHub-hosted runner, and sets the minimum OS to
iOS 26.0. Each package records the exact Rust, Xcode, SDK, and commit versions in `build-metadata.json` so the
resolved build inputs can be checked against the first green run and any later toolchain changes.

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
