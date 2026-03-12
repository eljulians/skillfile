/// Integration tests for `skillfile search` output formatting.
///
/// Exercises the formatting pipeline (print_table, print_json) with
/// fabricated data — no network, no GitHub token needed.
///
/// Run with: cargo test --test search
use skillfile::commands::search::{print_json, print_table};
use skillfile_sources::registry::{RegistryId, SearchResponse, SearchResult};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sample_response() -> SearchResponse {
    SearchResponse {
        total: 2,
        items: vec![
            SearchResult {
                name: "code-reviewer".to_string(),
                owner: "alice".to_string(),
                description: Some("Review code changes".to_string()),
                security_score: Some(92),
                stars: Some(150),
                url: "https://agentskill.sh/@alice/code-reviewer".to_string(),
                registry: RegistryId::AgentskillSh,
                source_repo: Some("alice/code-reviewer".to_string()),
                source_path: None,
            },
            SearchResult {
                name: "pr-review".to_string(),
                owner: "bob".to_string(),
                description: None,
                security_score: None,
                stars: None,
                url: "https://agentskill.sh/@bob/pr-review".to_string(),
                registry: RegistryId::AgentskillSh,
                source_repo: Some("bob/pr-review".to_string()),
                source_path: None,
            },
        ],
    }
}

fn multi_registry_response() -> SearchResponse {
    SearchResponse {
        total: 3,
        items: vec![
            SearchResult {
                name: "code-reviewer".to_string(),
                owner: "alice".to_string(),
                description: Some("Review code changes".to_string()),
                security_score: Some(92),
                stars: Some(150),
                url: "https://agentskill.sh/@alice/code-reviewer".to_string(),
                registry: RegistryId::AgentskillSh,
                source_repo: Some("alice/code-reviewer".to_string()),
                source_path: None,
            },
            SearchResult {
                name: "docker-helper".to_string(),
                owner: "dockerfan".to_string(),
                description: None,
                security_score: None,
                stars: Some(500),
                url: "https://skills.sh/dockerfan/docker-helper/docker-helper".to_string(),
                registry: RegistryId::SkillsSh,
                source_repo: Some("dockerfan/docker-helper".to_string()),
                source_path: None,
            },
            SearchResult {
                name: "testing-pro".to_string(),
                owner: "testmaster".to_string(),
                description: Some("Advanced testing".to_string()),
                security_score: Some(88),
                stars: Some(75),
                url: "https://www.skillhub.club/skills/testing-pro".to_string(),
                registry: RegistryId::SkillhubClub,
                source_repo: None,
                source_path: None,
            },
        ],
    }
}

// ---------------------------------------------------------------------------
// print_table tests
// ---------------------------------------------------------------------------

#[test]
fn table_single_registry_shows_via_label() {
    let resp = sample_response();
    let mut buf = Vec::new();
    print_table(&mut buf, &resp, Some("agentskill.sh"));
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("via agentskill.sh"));
    assert!(out.contains("code-reviewer"));
    assert!(out.contains("Review code changes"));
    assert!(out.contains("by alice"));
    assert!(out.contains("150 stars"));
    assert!(out.contains("score: 92/100"));
}

#[test]
fn table_single_registry_omits_registry_tag() {
    let resp = sample_response();
    let mut buf = Vec::new();
    print_table(&mut buf, &resp, Some("agentskill.sh"));
    let out = String::from_utf8(buf).unwrap();
    assert!(!out.contains("[agentskill.sh]"));
}

#[test]
fn table_multi_registry_shows_tags_and_label() {
    let resp = multi_registry_response();
    let mut buf = Vec::new();
    print_table(&mut buf, &resp, None);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("[agentskill.sh]"));
    assert!(out.contains("[skills.sh]"));
    assert!(out.contains("[skillhub.club]"));
    assert!(out.contains("across all registries"));
}

#[test]
fn table_empty_results() {
    let resp = SearchResponse {
        total: 0,
        items: vec![],
    };
    let mut buf = Vec::new();
    print_table(&mut buf, &resp, None);
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("No results found."));
}

#[test]
fn table_shows_total_when_more() {
    let resp = SearchResponse {
        total: 50,
        items: vec![SearchResult {
            name: "test".to_string(),
            owner: "owner".to_string(),
            description: Some("A test skill".to_string()),
            security_score: Some(80),
            stars: Some(10),
            url: "https://agentskill.sh/@owner/test".to_string(),
            registry: RegistryId::AgentskillSh,
            source_repo: None,
            source_path: None,
        }],
    };
    let mut buf = Vec::new();
    print_table(&mut buf, &resp, Some("agentskill.sh"));
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("1 result shown (50 total, via agentskill.sh)"));
}

#[test]
fn table_result_without_optional_fields() {
    let resp = SearchResponse {
        total: 1,
        items: vec![SearchResult {
            name: "minimal".to_string(),
            owner: String::new(),
            description: None,
            security_score: None,
            stars: None,
            url: "https://agentskill.sh/@x/minimal".to_string(),
            registry: RegistryId::AgentskillSh,
            source_repo: None,
            source_path: None,
        }],
    };
    let mut buf = Vec::new();
    print_table(&mut buf, &resp, Some("agentskill.sh"));
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("minimal"));
    assert!(out.contains("agentskill.sh/@x/minimal"));
    assert!(!out.contains("by "));
    assert!(!out.contains("stars"));
    assert!(!out.contains("score:"));
}

// ---------------------------------------------------------------------------
// print_json tests
// ---------------------------------------------------------------------------

#[test]
fn json_outputs_valid_json_with_registry() {
    let resp = sample_response();
    let mut buf = Vec::new();
    print_json(&mut buf, &resp).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(parsed["items"].is_array());
    assert!(parsed["total"].is_number());
    for item in parsed["items"].as_array().unwrap() {
        assert!(item["registry"].is_string());
    }
}

#[test]
fn json_empty() {
    let resp = SearchResponse {
        total: 0,
        items: vec![],
    };
    let mut buf = Vec::new();
    print_json(&mut buf, &resp).unwrap();
    let out = String::from_utf8(buf).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(parsed["total"], 0);
    assert!(parsed["items"].as_array().unwrap().is_empty());
}

#[test]
fn json_multi_registry_includes_all_tags() {
    let resp = multi_registry_response();
    let mut buf = Vec::new();
    print_json(&mut buf, &resp).unwrap();
    let out = String::from_utf8(buf).unwrap();
    assert!(out.contains("\"registry\": \"agentskill.sh\""));
    assert!(out.contains("\"registry\": \"skills.sh\""));
    assert!(out.contains("\"registry\": \"skillhub.club\""));
}
