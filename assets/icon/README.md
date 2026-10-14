# Icon

`AppIcon.icon` is the source: an Icon Composer document (open it in Xcode's Icon Composer to
edit). It holds the Strata glyph (`Assets/strata.svg`, white) on a steel-blue gradient. macOS 26
draws the light, dark, clear and tinted looks from it; the build scripts compile it with `actool`
into `Assets.car` plus an `AppIcon.icns` fallback for older macOS.

`xenon.png` / `xenon-512.png` are flat renders of the light look, used as the Dock icon when
running the bare binary (`cargo run`) and in the README.
