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
    ///
    /// `running` means the pane process is alive, not that the agent is
    /// input-ready.
    List {
        /// Project id from `~/.config/kira-mux/projects/<id>.toml`.
        project_id: String,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Show one agent's capabilities and live state.
    Capabilities {
        /// Project id from `~/.config/kira-mux/projects/<id>.toml`.
        project_id: String,
        /// Agent id within the project.
        agent_id: String,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Show the members of a named agent group.
    Group {
        /// Project id from `~/.config/kira-mux/projects/<id>.toml`.
        project_id: String,
        /// Group name as declared on agents under `groups`.
        group_name: String,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },
}
