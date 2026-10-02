# System One adapter for Rust

Ask TypeSafe System One questions of an OpenAI, Anthropic or Gemini model instead of the TypeSafe
API: the same prepared questions go to the model, and its reply comes back as the answers the
`typesafe-sdk-rust` crate returns, with probabilities, usage and a trace of every attempt. It is a
port of the Python package
[system-one-adapter-python](https://github.com/typesafe-ai/system-one-adapter-python), useful for
comparing System One against a general-purpose model on cost, speed and quality.
