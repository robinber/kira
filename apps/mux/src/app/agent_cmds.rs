//! Agent listing, send, and capture handlers (no durable bus).

use anyhow::Result;

use crate::cli::{AgentsArgs, AgentsCommand};
use crate::config::{EnvResolutionMode, load_project};
use crate::error::AiMuxError;
use crate::output;
use crate::paths::AppPaths;
use crate::tmux::TmuxClient;

pub(super) fn resolve_agents_args(args: AgentsArgs) -> Result<AgentsCommand> {
    if let Some(sub) = args.command {
        return Ok(sub);
    }
    let project_id = args.legacy.project_id.ok_or_else(|| {
        AiMuxError::MissingArgument(
            "project id is required\n\nUsage: kira-mux agents <PROJECT_ID> or kira-mux agents list <PROJECT_ID>".into(),
        )
    })?;
    Ok(AgentsCommand::List {
        project_id,
        profile: args.legacy.profile,
        json: args.legacy.json,
    })
}

pub(super) fn cmd_agents_dispatch(sub: AgentsCommand) -> Result<()> {
    let (project_id, profile) = match &sub {
        AgentsCommand::List {
            project_id,
            profile,
            ..
        }
        | AgentsCommand::Capabilities {
            project_id,
            profile,
            ..
        }
        | AgentsCommand::Group {
            project_id,
            profile,
            ..
        } => (project_id.as_str(), profile.as_deref()),
    };
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Deferred)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    let topology = crate::inspector::inspect(&tmux, &project)?;
    let agents_output = crate::domain::build_agents_output(&project, &topology);

    match sub {
        AgentsCommand::List { json, .. } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&agents_output)?);
            } else {
                output::print_agents_table(&agents_output);
            }
        }
        AgentsCommand::Capabilities { agent_id, json, .. } => {
            let agent = agents_output
                .agents
                .iter()
                .find(|a| a.id == agent_id)
                .ok_or_else(|| AiMuxError::UnknownAgentId(agent_id.clone()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output::AgentCapabilitiesOutput::from(agent))?
                );
            } else {
                output::print_agent_capabilities(agent);
            }
        }
        AgentsCommand::Group {
            group_name, json, ..
        } => {
            let members = agents_output
                .groups
                .get(&group_name)
                .ok_or_else(|| AiMuxError::UnknownGroupName(group_name.clone()))?;
            let group_members: Vec<_> = members
                .iter()
                .filter_map(|id| agents_output.agents.iter().find(|a| &a.id == id))
                .collect();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&output::GroupOutput::new(
                        &group_name,
                        &group_members
                    ))?
                );
            } else {
                output::print_group(&group_name, &group_members);
            }
        }
    }
    Ok(())
}

pub(super) fn cmd_send(
    project_id: &str,
    profile: Option<&str>,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Deferred)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    let prepared = crate::agent_io::prepare_prompt(&tmux, &project, agent_id, prompt, no_template)?;
    crate::agent_io::send_rendered_prompt(&tmux, &project, agent_id, &prepared.final_prompt)
}

pub(super) fn cmd_capture(
    project_id: &str,
    profile: Option<&str>,
    agent_id: &str,
    lines: usize,
    json: bool,
) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Deferred)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    let capture = crate::agent_io::capture_output(&tmux, &project, agent_id, lines)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&capture)?);
    } else {
        print!("{}", capture.output);
        if !capture.output.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}
