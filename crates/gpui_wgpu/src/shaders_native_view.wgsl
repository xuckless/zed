// --- native view holes --- //

// A native view shows through a hole: its bounds, clipped to its content mask, replace
// whatever was drawn beneath them with transparent black (Chromium's
// ReplaceExistingQuadWithHolePunch). The pipeline does not blend, so the zero replaces
// the color, and its depth test keeps opaque quads painted after the view on top. Holes
// are passed as quads, so both instance transports already decode them.

@vertex
fn vs_native_view_hole(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance_id: u32) -> @builtin(position) vec4<f32> {
    let hole = load_quad(instance_id);
    let unit_vertex = vec2<f32>(f32(vertex_id & 1u), 0.5 * f32(vertex_id & 2u));
    let clipped = clip_to_mask(unit_vertex, hole.bounds, hole.content_mask);
    return to_device_position_impl(clipped.position);
}

@fragment
fn fs_native_view_hole() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}
