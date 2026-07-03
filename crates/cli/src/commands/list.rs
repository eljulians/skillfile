use std::path::Path;

use serde::Serialize;
use skillfile_core::error::SkillfileError;
use skillfile_core::models::{EntityType, Entry, Manifest, SourceFields};
use skillfile_core::parser::MANIFEST_NAME;

pub struct ListOptions {
    pub json: bool,
    pub names_only: bool,
    pub skills: bool,
    pub agents: bool,
}

#[derive(Serialize)]
struct ListEntry<'a> {
    name: &'a str,
    entity_type: &'a str,
    source_type: &'a str,
    location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ref_: Option<&'a str>,
}

#[derive(Serialize)]
struct ListOutput<'a> {
    entries: Vec<ListEntry<'a>>,
    install_targets: Vec<String>,
}

fn include_entity(entity_type: EntityType, opts: &ListOptions) -> bool {
    match (opts.skills, opts.agents) {
        (false, false) | (true, true) => true,
        (true, false) => entity_type == EntityType::Skill,
        (false, true) => entity_type == EntityType::Agent,
    }
}

fn filtered_entries<'a>(manifest: &'a Manifest, opts: &ListOptions) -> Vec<&'a Entry> {
    manifest
        .entries
        .iter()
        .filter(|entry| include_entity(entry.entity_type, opts))
        .collect()
}

fn source_location_and_ref(source: &SourceFields) -> (String, Option<&str>) {
    match source {
        SourceFields::Github {
            owner_repo,
            path_in_repo,
            ref_,
        }
        | SourceFields::Gitlab {
            owner_repo,
            path_in_repo,
            ref_,
        } => (format!("{owner_repo}:{path_in_repo}"), Some(ref_.as_str())),
        SourceFields::Local { path } => (path.clone(), None),
        SourceFields::Url { url } => (url.clone(), None),
    }
}

fn source_location(source: &SourceFields) -> String {
    source_location_and_ref(source).0
}

fn print_group(title: &str, entries: &[&Entry]) {
    println!("{title} ({}):", entries.len());
    for entry in entries {
        let location = source_location(&entry.source);
        let ref_text = source_location_and_ref(&entry.source)
            .1
            .map_or(String::new(), |ref_| format!("  {ref_}"));
        println!(
            "  {:<16} {:<7} {}{}",
            entry.name,
            entry.source_type(),
            location,
            ref_text
        );
    }
}

fn print_human(manifest: &Manifest, entries: &[&Entry], opts: &ListOptions) {
    if entries.is_empty() {
        println!("No entries in {MANIFEST_NAME}.");
    } else {
        let skills: Vec<&Entry> = entries
            .iter()
            .copied()
            .filter(|entry| entry.entity_type == EntityType::Skill)
            .collect();
        let agents: Vec<&Entry> = entries
            .iter()
            .copied()
            .filter(|entry| entry.entity_type == EntityType::Agent)
            .collect();

        if !skills.is_empty() || opts.skills {
            print_group("Skills", &skills);
        }
        if !skills.is_empty() && !agents.is_empty() {
            println!();
        }
        if !agents.is_empty() || opts.agents {
            print_group("Agents", &agents);
        }
    }

    if !manifest.install_targets.is_empty() {
        let targets: Vec<String> = manifest
            .install_targets
            .iter()
            .map(ToString::to_string)
            .collect();
        println!("\nInstall targets: {}", targets.join(", "));
    }
}

fn print_names_only(entries: &[&Entry]) {
    for entry in entries {
        println!("{}", entry.name);
    }
}

fn print_json(manifest: &Manifest, entries: &[&Entry]) -> Result<(), SkillfileError> {
    let entries = entries
        .iter()
        .map(|entry| {
            let (location, ref_) = source_location_and_ref(&entry.source);
            ListEntry {
                name: &entry.name,
                entity_type: entry.entity_type.as_str(),
                source_type: entry.source_type(),
                location,
                ref_,
            }
        })
        .collect();
    let install_targets = manifest
        .install_targets
        .iter()
        .map(ToString::to_string)
        .collect();
    let output = ListOutput {
        entries,
        install_targets,
    };
    let json = serde_json::to_string_pretty(&output)
        .map_err(|e| SkillfileError::Manifest(format!("failed to serialize list output: {e}")))?;
    println!("{json}");
    Ok(())
}

pub fn cmd_list(repo_root: &Path, opts: &ListOptions) -> Result<(), SkillfileError> {
    let manifest_path = repo_root.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Err(SkillfileError::Manifest(format!(
            "{MANIFEST_NAME} not found in {}. Create one and run `skillfile init`.",
            repo_root.display()
        )));
    }

    let manifest = crate::config::parse_and_resolve(&manifest_path)?;
    let entries = filtered_entries(&manifest, opts);

    if opts.json {
        print_json(&manifest, &entries)
    } else if opts.names_only {
        print_names_only(&entries);
        Ok(())
    } else {
        print_human(&manifest, &entries, opts);
        Ok(())
    }
}
