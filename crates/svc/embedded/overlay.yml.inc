- insert:
    - id: svc-tools
      name: /home/hacker/hackmit2026/harness/svc-tools.mjs
      config: {}

- id: llm-pi-ai
  config:
    providers:
      anthropic:
        apiKeyEnv: ANTHROPIC_API_KEY
        api: anthropic-messages
        models:
          - id: claude-sonnet-4-5
            contextWindow: 200000
            maxTokens: 8192
      openai:
        apiKeyEnv: OPENAI_API_KEY
        api: openai-responses
        models:
          - id: gpt-5
            contextWindow: 400000
            maxTokens: 8192
      xai:
        apiKeyEnv: XAI_API_KEY
        api: openai-completions
        baseURL: https://api.x.ai/v1
        models:
          - id: grok-4
            contextWindow: 256000
            maxTokens: 8192
      openrouter:
        apiKeyEnv: OPENROUTER_API_KEY
        api: openai-completions
        baseURL: https://openrouter.ai/api/v1
        models:
          - id: deepseek/deepseek-chat
            contextWindow: 64000
            maxTokens: 8192

- id: acp
  inject:
    - acpAppStartup
  config:
    provider: openrouter
    model: deepseek/deepseek-chat

- id: agent-default-model
  config:
    provider: openrouter
    model: deepseek/deepseek-chat

- id: session-log-deepseek
  config:
    enabled: false

- id: tool-bash
  disabled: true
- id: tool-pwsh
  disabled: true
- id: tool-jobs
  disabled: true
- id: tool-skill
  disabled: true
- id: tool-subagent
  disabled: true
- id: tool-subagent-fork
  disabled: true
- id: tool-subagent-control
  disabled: true
- id: tool-subagent-list-agents
  disabled: true
- id: tool-workflow
  disabled: true
- id: tool-todo
  disabled: true
- id: tool-goal
  disabled: true
- id: tool-ralph
  disabled: true
- id: tool-web
  disabled: true
- id: web
  disabled: true
- id: web-search-deepseek
  disabled: true
- id: web-fetch-http
  disabled: true
- id: plan-mode
  disabled: true
- id: repeat-tool-reminder
  disabled: true
