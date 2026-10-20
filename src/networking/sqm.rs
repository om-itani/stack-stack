//! Bufferbloat control: CAKE on egress and, via an intermediate device, on
//! ingress.
//!
//! Egress shaping is simply a CAKE qdisc on the WAN interface. Ingress cannot be
//! shaped in place, so the standard trick is used: mirror everything arriving on
//! the WAN into an `ifb` device and give *that* a CAKE qdisc. Both directions
//! then have a single queue with fair queueing, so a bulk transfer cannot add
//! hundreds of milliseconds to a video call.

use std::fmt::Write;

use crate::networking::model::NetworkConfig;

/// One `program [args…]` invocation.
pub type Command = (String, Vec<String>);

/// CAKE parameters shared by both directions.
fn cake_args(download: bool, config: &NetworkConfig) -> Vec<String> {
    let sqm = &config.sqm;
    let bandwidth = if download {
        sqm.download_kbps
    } else {
        sqm.upload_kbps
    };

    let mut args = vec!["bandwidth".to_string(), format!("{bandwidth}kbit")];
    if sqm.diffserv4 {
        args.push("diffserv4".to_string());
    }
    // Egress is per-source fair, ingress is per-destination fair.
    args.push(if download {
        "dual-dsthost".to_string()
    } else {
        "dual-srchost".to_string()
    });
    if sqm.overhead != 0 {
        args.push("overhead".to_string());
        args.push(sqm.overhead.to_string());
    }
    if sqm.link_layer != "none" {
        args.push("linklayer".to_string());
        args.push(sqm.link_layer.clone());
    }
    args
}

/// Commands that install the shaping, in order. Idempotent: every qdisc/filter
/// uses `replace`, so re-running an apply is safe.
pub fn apply_commands(config: &NetworkConfig) -> Vec<Command> {
    let wan = &config.sqm.interface;
    let ifb = &config.sqm.ifb;

    let mut commands: Vec<Command> = Vec::with_capacity(6);

    let mut egress = vec![
        "qdisc".to_string(),
        "replace".to_string(),
        "dev".to_string(),
        wan.clone(),
        "root".to_string(),
        "cake".to_string(),
    ];
    egress.extend(cake_args(false, config));
    commands.push(("tc".to_string(), egress));

    // Ingress: mirror WAN traffic into the ifb device, shape there.
    commands.push(("modprobe".to_string(), vec!["ifb".to_string()]));
    commands.push((
        "ip".to_string(),
        vec![
            "link".to_string(),
            "set".to_string(),
            ifb.clone(),
            "up".to_string(),
        ],
    ));
    commands.push((
        "tc".to_string(),
        vec![
            "qdisc".to_string(),
            "replace".to_string(),
            "dev".to_string(),
            wan.clone(),
            "handle".to_string(),
            "ffff:".to_string(),
            "ingress".to_string(),
        ],
    ));
    commands.push((
        "tc".to_string(),
        vec![
            "filter".to_string(),
            "replace".to_string(),
            "dev".to_string(),
            wan.clone(),
            "parent".to_string(),
            "ffff:".to_string(),
            "protocol".to_string(),
            "all".to_string(),
            "u32".to_string(),
            "match".to_string(),
            "u32".to_string(),
            "0".to_string(),
            "0".to_string(),
            "action".to_string(),
            "mirred".to_string(),
            "egress".to_string(),
            "redirect".to_string(),
            "dev".to_string(),
            ifb.clone(),
        ],
    ));

    let mut ingress = vec![
        "qdisc".to_string(),
        "replace".to_string(),
        "dev".to_string(),
        ifb.clone(),
        "root".to_string(),
        "cake".to_string(),
    ];
    ingress.extend(cake_args(true, config));
    commands.push(("tc".to_string(), ingress));

    commands
}

/// Tear the shaping back down. Every command is best-effort: removing a qdisc
/// that is not there is not an error worth surfacing.
pub fn clear_commands(config: &NetworkConfig) -> Vec<Command> {
    let wan = &config.sqm.interface;
    let ifb = &config.sqm.ifb;

    vec![
        (
            "tc".to_string(),
            vec![
                "qdisc".to_string(),
                "del".to_string(),
                "dev".to_string(),
                wan.clone(),
                "root".to_string(),
            ],
        ),
        (
            "tc".to_string(),
            vec![
                "qdisc".to_string(),
                "del".to_string(),
                "dev".to_string(),
                wan.clone(),
                "ingress".to_string(),
            ],
        ),
        (
            "tc".to_string(),
            vec![
                "qdisc".to_string(),
                "del".to_string(),
                "dev".to_string(),
                ifb.clone(),
                "root".to_string(),
            ],
        ),
    ]
}

/// Shell-rendered form of [`apply_commands`], for the UI preview.
pub fn script(config: &NetworkConfig) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "#!/bin/sh");
    let _ = writeln!(
        out,
        "# Generated by stack-stack. Manual edits are overwritten."
    );
    for (program, args) in apply_commands(config) {
        let _ = writeln!(out, "{program} {}", args.join(" "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::networking::model::{InterfaceConfig, InterfaceRole, SqmConfig};

    fn sqm_config() -> NetworkConfig {
        NetworkConfig {
            interfaces: vec![InterfaceConfig {
                name: "eth0".to_string(),
                role: InterfaceRole::Wan,
                address: None,
            }],
            sqm: SqmConfig {
                enabled: true,
                interface: "eth0".to_string(),
                download_kbps: 80_000,
                upload_kbps: 20_000,
                link_layer: "ethernet".to_string(),
                overhead: 38,
                diffserv4: true,
                ifb: "ifb0".to_string(),
            },
            ..NetworkConfig::default()
        }
    }

    #[test]
    fn egress_and_ingress_are_both_queued() {
        let commands = apply_commands(&sqm_config());
        let rendered: Vec<String> = commands
            .iter()
            .map(|(program, args)| format!("{program} {}", args.join(" ")))
            .collect();

        assert_eq!(
            rendered[0],
            "tc qdisc replace dev eth0 root cake bandwidth 20000kbit diffserv4 dual-srchost overhead 38 linklayer ethernet"
        );
        assert!(rendered.iter().any(|line| line == "modprobe ifb"));
        assert!(rendered.iter().any(|line| line == "ip link set ifb0 up"));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("handle ffff: ingress"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("action mirred egress redirect dev ifb0"))
        );
        assert_eq!(
            rendered.last().expect("last"),
            "tc qdisc replace dev ifb0 root cake bandwidth 80000kbit diffserv4 dual-dsthost overhead 38 linklayer ethernet"
        );
    }

    #[test]
    fn optional_cake_arguments_are_omitted_when_defaulted() {
        let mut config = sqm_config();
        config.sqm.overhead = 0;
        config.sqm.link_layer = "none".to_string();
        config.sqm.diffserv4 = false;
        let commands = apply_commands(&config);
        let egress = format!("{} {}", commands[0].0, commands[0].1.join(" "));
        assert_eq!(
            egress,
            "tc qdisc replace dev eth0 root cake bandwidth 20000kbit dual-srchost"
        );
    }

    #[test]
    fn every_install_command_is_idempotent() {
        // Re-applying must not fail on an already-configured interface.
        for (program, args) in apply_commands(&sqm_config()) {
            if program == "tc" {
                assert!(
                    matches!(args[1].as_str(), "replace"),
                    "expected replace, got {program} {}",
                    args.join(" ")
                );
            }
        }
    }

    #[test]
    fn clearing_removes_both_directions() {
        let commands = clear_commands(&sqm_config());
        let rendered: Vec<String> = commands
            .iter()
            .map(|(program, args)| format!("{program} {}", args.join(" ")))
            .collect();
        assert_eq!(rendered[0], "tc qdisc del dev eth0 root");
        assert_eq!(rendered[1], "tc qdisc del dev eth0 ingress");
        assert_eq!(rendered[2], "tc qdisc del dev ifb0 root");
    }
}
