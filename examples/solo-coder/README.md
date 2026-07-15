# Solo coder example

A minimal Kira project: two panes (a coder agent and a watch-mode test shell).

1. Copy [`project.toml`](./project.toml) to
   `~/.config/kira-mux/projects/solo-coder.toml`
2. Set `root` to a real project directory
3. Run:

```bash
# Attach so you can finish any first-run agent UI (trust dir, login, …)
kira-mux open solo-coder
# detach when the coder pane is ready for tasks, then:
kira-mux send solo-coder coder "summarize the repo layout"
kira-mux capture solo-coder coder --lines 80
```

`status` showing `running` only means the pane process is alive — not that the
agent is past setup. See the main README section **Running vs input-ready**.
