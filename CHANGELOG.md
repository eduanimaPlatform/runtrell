# Changelog

## Unreleased

- Fix OpenAI Responses and chat completions (including OpenRouter) rejecting
  logical tool names such as `files.list`. Definitions and historical calls now
  use deterministic names within the provider's 64-character ASCII constraint;
  returned calls restore the original logical names. Distinct names that collide
  produce a configuration error before sending. Gemini names remain unchanged.
