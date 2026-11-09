//! The browser draws with the same shader (`@cha/pyrowave-webgpu`'s copy): keep them one file.

#[test]
fn the_browser_has_the_same_shader() {
    assert!(
        cha_upscale::SHADER
            == include_str!("../../../web/packages/pyrowave-webgpu/src/shaders/sgsr1.wgsl"),
        "copy crates/cha-upscale/src/sgsr1.wgsl to web/packages/pyrowave-webgpu/src/shaders/sgsr1.wgsl"
    );
}
