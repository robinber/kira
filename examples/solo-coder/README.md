# Solo coder example

A minimal Kira project: two panes (a coder agent and a watch-mode test shell).

1. Copy [`project.toml`](./project.toml) to
   `~/.config/kira-mux/projects/solo-coder.toml`
2. Set `root` to a real project directory
3. Run:

```bash
kira-mux open solo-coder
kira-mux send solo-coder coder "summarize the repo layout"
kira-mux capture solo-coder coder --lines 80
```
