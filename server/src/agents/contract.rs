//! Structural contract tests for the agent extension point. The registry, the
//! adapter trait surface, and the per-kind behaviour are fixed here so that
//! adding an agent type cannot silently change them.

use std::collections::HashSet;

use crate::models::{
    AgentRow, AGENT_CLI, AGENT_CODEX, AGENT_MOCK, AGENT_OPENCODE, AGENT_WORKBUDDY,
};
use crate::usage::UsageRecord;

use super::*;

/// Job id whose payload is a UUID; adapters derive a UUID-shaped session from it.
const JOB_ID: &str = "job_123e4567-e89b-12d3-a456-426614174000";

/// JSONL a Codex CLI emits for one turn. Token values match the codex case in
/// tests/test_agent_adapters.py.
const CODEX_JSONL: &str = concat!(
    "{\"type\":\"thread.started\",\"thread_id\":\"thread-agts\"}\n",
    "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1234,",
    "\"cached_input_tokens\":100,\"output_tokens\":567,",
    "\"reasoning_output_tokens\":89}}\n"
);

/// JSONL a WorkBuddy CLI emits for one turn. Token and cost values match the
/// workbuddy case in tests/test_agent_adapters.py.
const WORKBUDDY_JSONL: &str = concat!(
    "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-agts\"}\n",
    "{\"type\":\"assistant\",\"session_id\":\"session-agts\",",
    "\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",",
    "\"text\":\"已写入 domo_mock/T1.txt 与 result.json\"}]}}\n",
    "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,",
    "\"total_cost_usd\":0.0123,\"usage\":{\"input_tokens\":2048,",
    "\"cache_read_input_tokens\":200,\"cache_creation_input_tokens\":50,",
    "\"output_tokens\":512}}\n"
);

/// Adapter sources, read at compile time so the structural assertions do not
/// depend on the test process working directory.
const ADAPTER_SOURCES: [&str; 5] = [
    include_str!("mock.rs"),
    include_str!("opencode.rs"),
    include_str!("cli.rs"),
    include_str!("codex.rs"),
    include_str!("workbuddy.rs"),
];

fn is_uuid_shape(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(part, len)| part.len() == len && part.chars().all(|ch| ch.is_ascii_hexdigit()))
}

fn finish_context<'a>(
    agent: &'a AgentRow,
    job_id: &'a str,
    shutting_down: bool,
) -> FinishContext<'a> {
    FinishContext {
        agent,
        job_id,
        session_id: None,
        process_failed: false,
        shutting_down,
    }
}

#[test]
fn registry_matches_adapter_kinds_in_declared_order() {
    let declared = [
        <MockAdapter as AgentAdapter>::KIND,
        <OpencodeAdapter as AgentAdapter>::KIND,
        <CodexAdapter as AgentAdapter>::KIND,
        <WorkbuddyAdapter as AgentAdapter>::KIND,
        <CliAdapter as AgentAdapter>::KIND,
    ];
    assert_eq!(
        declared,
        [
            AGENT_MOCK,
            AGENT_OPENCODE,
            AGENT_CODEX,
            AGENT_WORKBUDDY,
            AGENT_CLI
        ]
    );
    assert_eq!(kinds(), declared.as_slice());
    // The registry is a stable slice, not rebuilt per call.
    assert_eq!(kinds(), declared.as_slice());

    let unique: HashSet<&&str> = kinds().iter().collect();
    assert_eq!(unique.len(), kinds().len());
    assert_eq!(kinds().len(), 5);
}

#[test]
fn only_the_cli_adapter_requires_a_command_template() {
    for kind in kinds() {
        assert_eq!(
            requires_command_template(kind),
            *kind == AGENT_CLI,
            "kind {kind}"
        );
    }
    assert!(<CliAdapter as AgentAdapter>::requires_command_template(
        &AgentConfig::default()
    ));
    for kind in [AGENT_MOCK, AGENT_OPENCODE, AGENT_CODEX, AGENT_WORKBUDDY] {
        assert!(
            !requires_command_template(kind),
            "kind {kind} 不应要求命令模板"
        );
    }
    assert!(!requires_command_template("unknown"));
}

#[test]
fn cli_validation_rejects_an_empty_template_and_others_pass() {
    let empty = AgentConfig {
        kind: AGENT_CLI.into(),
        ..AgentConfig::default()
    };
    let error = CliAdapter::validate(&empty).unwrap_err();
    assert_eq!(error.to_string(), "CLI Agent 必须配置命令模板");
    assert!(validate_config(AGENT_CLI, &[]).is_err());

    let configured = AgentConfig {
        command: vec!["sh".into()],
        ..empty
    };
    assert!(CliAdapter::validate(&configured).is_ok());
    assert!(validate_config(AGENT_CLI, &["sh".to_string()]).is_ok());

    for kind in [AGENT_MOCK, AGENT_OPENCODE, AGENT_CODEX, AGENT_WORKBUDDY] {
        assert!(validate_config(kind, &[]).is_ok(), "kind {kind}");
    }
}

#[test]
fn sessions_follow_the_adapter_contract() {
    let opencode =
        session_for_job(&AgentRow::test_fixture(AGENT_OPENCODE), JOB_ID).expect("opencode 会话");
    assert!(opencode.starts_with("ses_domo"), "{opencode}");
    assert_eq!(opencode, crate::opencode::session_id(JOB_ID));

    assert_eq!(
        session_for_job(&AgentRow::test_fixture(AGENT_MOCK), JOB_ID),
        None
    );

    // Every non-opencode kind either has no session (mock) or derives one that
    // is UUID-shaped, falling back to the ses_domo prefix for short job ids.
    for kind in kinds().iter().filter(|kind| **kind != AGENT_OPENCODE) {
        match session_for_job(&AgentRow::test_fixture(kind), JOB_ID) {
            None => assert_eq!(*kind, AGENT_MOCK, "kind {kind}"),
            Some(value) => assert!(
                is_uuid_shape(&value) || value.starts_with("ses_domo"),
                "kind {kind}: {value}"
            ),
        }
    }

    for kind in [AGENT_CLI, AGENT_CODEX, AGENT_WORKBUDDY] {
        let session = session_for_job(&AgentRow::test_fixture(kind), JOB_ID)
            .unwrap_or_else(|| panic!("kind {kind} 缺少会话"));
        assert_eq!(session, "123e4567-e89b-12d3-a456-426614174000");
        assert!(is_uuid_shape(&session), "kind {kind}: {session}");

        let fallback = session_for_job(&AgentRow::test_fixture(kind), "job_short")
            .unwrap_or_else(|| panic!("kind {kind} 缺少兜底会话"));
        assert_eq!(fallback, "ses_domoshort");
    }
}

#[tokio::test]
async fn finish_job_defaults_to_per_turn_and_opencode_is_authoritative() {
    for kind in [AGENT_MOCK, AGENT_CLI, AGENT_CODEX, AGENT_WORKBUDDY] {
        let agent = AgentRow::test_fixture(kind);
        let usage = finish_job(finish_context(&agent, JOB_ID, false)).await;
        assert_eq!(usage, JobUsage::PerTurn, "kind {kind}");
    }

    // A missing OpenCode binary keeps the settlement local: no ledger and no
    // API answer, so the adapter still reports an authoritative empty record.
    std::env::set_var("DOMO_OPENCODE_BIN", "/nonexistent/domo-opencode-fixture");
    let agent = AgentRow::test_fixture(AGENT_OPENCODE);
    let settled = finish_job(finish_context(&agent, JOB_ID, false)).await;
    let skipped = finish_job(finish_context(&agent, JOB_ID, true)).await;
    std::env::remove_var("DOMO_OPENCODE_BIN");

    assert!(
        matches!(settled, JobUsage::Authoritative(_)),
        "opencode 结算: {settled:?}"
    );
    assert_eq!(skipped, JobUsage::PerTurn);
}

#[test]
fn usage_settlement_keeps_merged_values_or_replaces_them() {
    let merged = Some(UsageRecord {
        input_tokens: Some(10),
        output_tokens: Some(4),
        cost: Some(0.5),
        ..UsageRecord::default()
    });
    let ledger = UsageRecord {
        input_tokens: Some(99),
        ..UsageRecord::default()
    };

    assert_eq!(apply_usage(merged.clone(), JobUsage::PerTurn), merged);
    assert_eq!(apply_usage(None, JobUsage::PerTurn), None);
    assert_eq!(
        apply_usage(
            merged.clone(),
            JobUsage::Authoritative(Some(ledger.clone()))
        ),
        Some(ledger.clone())
    );
    assert_eq!(apply_usage(merged, JobUsage::Authoritative(None)), None);
    assert_eq!(
        apply_usage(None, JobUsage::Authoritative(Some(ledger.clone()))),
        Some(ledger)
    );
}

#[test]
fn trait_surface_exposes_only_business_capabilities() {
    let trait_source = include_str!("adapter.rs");
    for hook in [
        concat!("fn ", "invocation"),
        concat!("fn ", "usage_from_stdout"),
        concat!("fn ", "session_after_run"),
        concat!("fn ", "initial_session_id"),
    ] {
        assert!(
            !trait_source.contains(hook),
            "trait 不应暴露低层钩子 {hook}"
        );
    }
    for capability in [
        "fn requires_command_template",
        "fn validate",
        "fn session_for_job",
        "fn run_turn",
        "fn finish_job",
    ] {
        assert!(
            trait_source.contains(capability),
            "trait 缺少业务能力 {capability}"
        );
    }
}

#[test]
fn adapters_do_not_call_runner_session_or_usage_decisions() {
    for source in ADAPTER_SOURCES {
        for forbidden in [
            "crate::runner",
            "merge_usage",
            "initial_session_id",
            "session_after_run",
            "usage_from_stdout",
            "agent.kind ==",
        ] {
            assert!(!source.contains(forbidden), "适配器不应引用 {forbidden}");
        }
    }
}

#[test]
fn runner_defers_session_and_usage_decisions_to_the_registry() {
    let runner = include_str!("../runner.rs");
    assert!(runner.contains("agents::session_for_job"));
    assert!(runner.contains("agents::finish_job"));
    assert!(runner.contains("agents::apply_usage"));
    assert!(!runner.contains("agent.kind =="));
    assert!(!runner.contains("initial_session_id"));
    assert!(!runner.contains("AGENT_OPENCODE"));
}

#[test]
fn codex_fixture_matches_the_end_to_end_usage_expectations() {
    let usage = super::codex::parse_codex_usage(CODEX_JSONL).expect("codex usage");
    assert_eq!(usage.input_tokens, Some(1234));
    assert_eq!(usage.output_tokens, Some(567));
    assert_eq!(usage.cache_read_tokens, Some(100));
    assert_eq!(usage.cache_write_tokens, None);
    assert_eq!(usage.reasoning_tokens, Some(89));
    assert_eq!(usage.cost, None);
    assert_eq!(
        super::codex::parse_codex_session_id(CODEX_JSONL).as_deref(),
        Some("thread-agts")
    );
}

#[test]
fn workbuddy_fixture_matches_the_end_to_end_usage_expectations() {
    let usage = super::workbuddy::parse_workbuddy_usage(WORKBUDDY_JSONL).expect("workbuddy usage");
    assert_eq!(usage.input_tokens, Some(2048));
    assert_eq!(usage.output_tokens, Some(512));
    assert_eq!(usage.cache_read_tokens, Some(200));
    assert_eq!(usage.cache_write_tokens, Some(50));
    assert_eq!(usage.reasoning_tokens, None);
    assert_eq!(usage.cost, Some(0.0123));
    assert_eq!(
        super::workbuddy::parse_workbuddy_session_id(WORKBUDDY_JSONL).as_deref(),
        Some("session-agts")
    );
}

#[test]
fn agent_config_reads_settings_from_the_row() {
    let mut row = AgentRow::test_fixture(AGENT_CLI);
    row.model = "gpt-test".into();
    row.command_json = Some(r#"["sh","-c","{prompt}"]"#.into());
    row.model_args_json = Some(r#"["--model","{model}"]"#.into());

    let config = AgentConfig::from_row(&row);
    assert_eq!(config.kind, AGENT_CLI);
    assert_eq!(config.model, "gpt-test");
    assert_eq!(config.command, ["sh", "-c", "{prompt}"]);
    assert_eq!(config.model_args, ["--model", "{model}"]);
}
