@compute @workgroup_size(8, 8, 1)
fn aviqtl_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let coord = vec2<i32>(id.xy);
    let uv = aviqtl_uv(coord);
    let texel = vec2<f32>(1.0) / vec2<f32>(aviqtl_size());
    let radius = i32(clamp(aviqtl_parameter(0u).x, 1.0, 12.0));
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    // Pass 0 blurs horizontally, pass 1 vertically: ping-pong makes the pair a
    // separable Gaussian, the same shape Qt's Blur compute effect used.
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let weight = 1.0 - abs(f32(offset)) / f32(radius + 1);
        var step = vec2<f32>(f32(offset) * texel.x, 0.0);
        if aviqtl_pass() != 0u {
            step = vec2<f32>(0.0, f32(offset) * texel.y);
        }
        sum = sum + aviqtl_texel(uv + step) * weight;
        total = total + weight;
    }
    aviqtl_write(coord, sum / max(total, 0.0001));
}
