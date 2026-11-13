use client::renderer::voxel_pull::gpu::{forward_cull_source, ForwardCullSource};
use client::renderer::voxel_pull::prepare::frustum_metric_contribution;

#[test]
fn missing_hzb_selects_frustum_as_the_forward_cull_source() {
    assert_eq!(forward_cull_source(false), ForwardCullSource::Frustum);
    assert_eq!(forward_cull_source(true), ForwardCullSource::Hzb);
}

#[test]
fn shadow_views_contribute_zero_to_the_frustum_metric() {
    assert_eq!(frustum_metric_contribution(7, false), 7);
    assert_eq!(frustum_metric_contribution(7, true), 0);
}
