use std::sync::atomic::AtomicI64;

#[derive(Default)]
pub struct SysMetrics {
    pub agent_ns: AtomicI64,
    pub mount_ns: AtomicI64,
    pub controller_ns: AtomicI64,
    pub character_behavior_ns: AtomicI64,
    pub stats_ns: AtomicI64,
    pub phys_ns: AtomicI64,
    pub projectile_ns: AtomicI64,
    pub combat_ns: AtomicI64,
}
