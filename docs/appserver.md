# Codex app-server support

`codex-bridge` provides a Web UI and CLI for continuing Codex sessions across devices. The
[official App Server documentation](https://learn.chatgpt.com/docs/app-server) describes the
underlying protocol. Available features depend on the Codex version connected to Bridge.

## Available in this project

- Browse projects and sessions, read long histories, inspect tool activity, and follow live work.
- Create, rename, pin, and archive sessions; choose a model and view account usage.
- Send and queue prompts, steer a running turn, withdraw queued prompts, and stop work.
- View and edit a Goal in a floating panel.
- Answer asynchronous clarification questions in a floating, nonblocking card. A reply joins the
  originating running turn; after that turn ends, the UI explicitly offers a new turn.
- Review diffs and local files, use editable voice transcription when a supported backend is
  available, and inspect managed-service status.

See the [installation guide](install.md), [Web UI streaming behavior](web-ui-streaming.md), and
[release notes](releases/) for user-facing details.
