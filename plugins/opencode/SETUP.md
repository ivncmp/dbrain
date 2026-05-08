# dBrain — OpenCode Plugin Setup

## 1. Copy plugin files

Copy `dbrain.ts` and `utils.ts` into your OpenCode plugins directory:

```bash
cp plugins/opencode/dbrain.ts ~/.config/opencode/plugins/
cp plugins/opencode/utils.ts ~/.config/opencode/plugins/
```

> OpenCode auto-loads any `.ts` file in `~/.config/opencode/plugins/`.

## 2. Add MCP connection to opencode.json

Add this entry under `"mcp"` in `~/.config/opencode/opencode.json`:

```json
{
  "dbrain": {
    "type": "remote",
    "url": "http://localhost:7878/mcp",
    "headers": {
      "Authorization": "Bearer {env:DBRAIN_TOKEN}"
    }
  }
}
```

## 3. Set your token

Export the token generated during `dbrain init`:

```bash
export DBRAIN_TOKEN="sk-dbr_your_token_here"
```

Add it to your shell profile (`.zshrc`, `.bashrc`, etc.) for persistence.

## 4. Verify

Start OpenCode in any project. The plugin will:

1. Auto-start dBrain if not already running
2. Inject memory protocol instructions into the system prompt
3. Log conversations to dBrain for future recall

You can verify the connection by asking the agent to call `wake_up` or `recall`.

## Environment variables (optional)

| Variable | Default | Description |
|----------|---------|-------------|
| `DBRAIN_PORT` | `7878` | dBrain server port |
| `DBRAIN_DATA` | `~/.dbrain` | Data directory path |
| `DBRAIN_TOKEN` | _(required)_ | Bearer token for auth |
