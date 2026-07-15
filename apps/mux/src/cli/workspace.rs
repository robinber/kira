use clap::Subcommand;

/// Arguments for `kira-mux agents`; a subcommand is required.
#[derive(Debug, clap::Args)]
pub(crate) struct AgentsArgs {
    #[command(subcommand)]
    pub(crate) command: AgentsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentsCommand {
    /// List configured agents and their runtime state.
    List {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one agent's capabilities and state.
    Capabilities {
        project_id: String,
        agent_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the members of a named agent group.
    Group {
        project_id: String,
        group_name: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
