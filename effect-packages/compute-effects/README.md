# Compute effects

Two compute-shader effects. `ComputeDesaturate.wgsl` mixes each pixel toward its
Rec. 709 luminance in one dispatch; `ComputeBlur.wgsl` declares `"dispatch": 2`
and blurs horizontally on pass 0 and vertically on pass 1, so the ping-pong
targets turn it into a separable Gaussian. Both advertise the same `workgroup`
tile in their `aviqtl-wgsl-v1` runtime, so the host dispatch covers the layer
texture without leftover pixels.

See the [native package contract](../README.md#compute-effects) for the helper
functions available in compute shaders.
