# We don't sell wires. We sell experience.

*Introducing **Ferridis** — a manifesto for the next layer of opinion above MCP.*

---

Right now, to get an AI assistant to read your calendar, you have to install a server, edit a JSON file, and hope a background process stays alive. To connect a second tool, you do it again. To share the setup with a colleague, you write documentation. We have built models that can reason across thousands of pages of legal text — and we ask their users to be sysadmins.

This is the wrong layer.

## What we believe

We believe the value of AI isn't in the model. It's in how seamlessly the model fits into the work you already do. The model is the engine. The connection to your world is the car. Nobody buys an engine.

Apple didn't sell music players. They sold a thousand songs in your pocket. Stripe didn't sell payment APIs. They sold seven lines of code instead of seven weeks of integration. The companies that win in any new technology layer are the ones that hide the technology and surface the experience.

MCP solved the connection problem at the wire level — and got industry consensus for it. The next problem is the experience around the wire. The car around the engine.

## The standard is settled. The layer above isn't.

In December 2025, Anthropic donated MCP to the Agentic AI Foundation — a Linux Foundation directed fund co-founded with Block and OpenAI. Google's A2A, an open standard for agent-to-agent communication, is a complementary protocol in the same ecosystem. The wire protocol for connecting AI to tools is now an industry-coalition standard, not anyone's private project. That is good news for users and good news for the ecosystem.

It also means the interesting work has moved up the stack. The wire is solved. What sits on top of it — connections instead of servers, federated discovery, lazy schemas, an intent vocabulary, a coherent fallback to the browser — is still wide open. That is where Ferridis lives. Not as a replacement for the standard, but as the opinionated layer above it: a reference design and a Rust implementation for what the experience around the wire could look like.

## How we'll do it differently

Instead of *servers* the user installs and runs, **Ferridis** gives them *connections* they authorize. A connection is a durable, consent-based link between the AI and a service — closer in feel to logging into an app on your phone than to configuring middleware. You authorize once. The connection self-describes what it can do. The AI uses it.

Underneath that user-facing simplicity, six layers do the work:

- **Connections** carry identity, trust, and consent.
- **A capability mesh** publishes what the world offers, and loads only what the AI needs in the moment — so the model's attention isn't crowded with tools it will never call.
- **Web-native specs** (OpenAPI, AsyncAPI) describe each capability — no new spec language to learn.
- **Bidirectional channels** let the world push events to the AI, not just the other way around.
- **An intent layer** turns goals ("tell Alice I'll be late") into the right capability call automatically.
- **The browser** stays as a universal fallback for anything not yet wired up.

The user sees none of this. They see one thing: *I connected my calendar. The assistant uses it.*

## Why now

Three things are true at once that weren't true two years ago. AI models are now capable enough that the limiting factor is integration, not intelligence. Every major service already exposes OAuth and an OpenAPI spec — the substrate is in place. And users have learned, through every install-a-server-per-tool experience, what the painful version of this looks like. Which means they're ready for the painless one.

The layer that hides the wires wins. Ferridis is built to be that layer — open, well-named, properly attributed.

## What this is

This is the start of a public design process for Ferridis. The architecture, the trade-offs, the decisions, the mistakes — all of it gets written down here, in the open. If you build for AI, integrate AI, or use AI seriously enough to feel the friction, follow along. Push back. Help shape it.

The work begins now.

— George Andrikopoulos
