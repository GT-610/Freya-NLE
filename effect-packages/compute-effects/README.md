# Compute effects

A compute-shader package: `ComputeDesaturate.wgsl` declares one
`@compute @workgroup_size(8, 8, 1) fn aviqtl_main` entry point and mixes each
pixel toward its Rec. 709 luminance. The metadata advertises the same
`workgroup` tile in its `aviqtl-wgsl-v1` runtime, so the host dispatch covers the
layer texture without leftover pixels.

See the [native package contract](../README.md#compute-effects) for the helper
functions available in compute shaders.
