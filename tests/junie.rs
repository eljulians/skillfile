/// Integration tests for Junie platform deployment.
///
/// These tests verify that Junie skills and agents deploy to the expected
/// directory structure:
/// - Skills: ./.junie/skills/<name>/SKILL.md (Nested mode)
/// - Agents: ./.junie/agents/<name>.md (Flat mode)
use skillfile_functional_tests::sf;

#[test]
fn install_junie_skill_nested() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create local skill file
    std::fs::create_dir_all(root.join("skills")).unwrap();
    std::fs::write(root.join("skills/my-skill.md"), "# My Skill\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  junie  local\n\
         local  skill  my-skill  skills/my-skill.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // Junie skills should be in .junie/skills/<name>/SKILL.md
    let deployed_skill = root.join(".junie/skills/my-skill/SKILL.md");
    assert!(
        deployed_skill.exists(),
        "Junie skill must deploy as .junie/skills/my-skill/SKILL.md"
    );
    assert_eq!(
        std::fs::read_to_string(&deployed_skill).unwrap(),
        "# My Skill\n"
    );
}

#[test]
fn install_junie_agent_flat() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create local agent file
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(root.join("agents/my-agent.md"), "# My Agent\n").unwrap();

    std::fs::write(
        root.join("Skillfile"),
        "install  junie  local\n\
         local  agent  my-agent  agents/my-agent.md\n",
    )
    .unwrap();

    sf(root).arg("install").assert().success();

    // Junie agents should be in .junie/agents/<name>.md
    let deployed_agent = root.join(".junie/agents/my-agent.md");
    assert!(
        deployed_agent.exists(),
        "Junie agent must deploy as .junie/agents/my-agent.md"
    );
    assert_eq!(
        std::fs::read_to_string(&deployed_agent).unwrap(),
        "# My Agent\n"
    );
}
