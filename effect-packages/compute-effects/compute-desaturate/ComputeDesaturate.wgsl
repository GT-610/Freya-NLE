@compute @workgroup_size(8, 8, 1)
fn aviqtl_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let coord = vec2<i32>(id.xy);
    let source = aviqtl_pixel(coord);
    let amount = clamp(aviqtl_parameter(0u).x, 0.0, 1.0);
    // Rec. 709 luminance keeps the desaturated result perceptually even.
    let luma = dot(source.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let grey = vec3<f32>(luma);
    aviqtl_write(coord, vec4<f32>(mix(source.rgb, grey, amount), source.a));
}
