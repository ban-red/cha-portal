# cha-moonlight-input

The browser input model as Moonlight input events, extracted from `cha-gateway` so the gateway (a browser's input over JSON) and the native client (`cha-client-gamestream`) share one tested table ([ADR 0010](../../docs/adr/0010-native-client-macos-first.md)).

- Input comes in as `cha_client::Input`: keys by W3C `KeyboardEvent.code`, absolute and relative mouse, `PointerEvent.button`, wheel pixels as a browser reports them, and pads in the Gamepad API's standard mapping.
- `InputState` (one per session) turns each into `ClientInputEvent`s for the host's control stream and remembers what is held, so `release_all()` can let go of keys, buttons and pads when a session loses focus or leaves. Wheel (100 px per notch is 120 units) and relative-motion remainders carry over; stick Y is inverted; pads connect as Xbox controllers with analog triggers and rumble.
- `keys` is the `KeyboardEvent.code` to Windows virtual-key table (US layout) with Moonlight's modifier and extended flags.
- `rumble_levels` turns a host's 16-bit motor values into 0..1.

`cargo test -p cha-moonlight-input` needs no host. Depends on `moonlight-common` (GPL-3.0-or-later, pinned as in the gateway) for the event types.
