// env 是进程级的，本文件只放这一个用例（独立二进制 = 无并行串扰）。

const ENV: &str = "PECO_MEMORY_CONSOLIDATION_ENABLED";

#[test]
fn consolidation_switch_reaches_default_config_via_env() {
    unsafe { std::env::remove_var(ENV) };
    let off = peco_server::peco::config::PecoConfig::default();
    assert!(
        !off.memory.consolidation.enabled,
        "未设置 env 时总开关必须关闭（fail-closed）"
    );

    unsafe { std::env::set_var(ENV, "true") };
    let on = peco_server::peco::config::PecoConfig::default();
    assert!(
        on.memory.consolidation.enabled,
        "PECO_MEMORY_CONSOLIDATION_ENABLED=true 应经 PecoConfig::default() 到达总开关"
    );

    unsafe { std::env::remove_var(ENV) };
    let back = peco_server::peco::config::PecoConfig::default();
    assert!(!back.memory.consolidation.enabled);
}
