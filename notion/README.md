# notion — Notion connector (remote MCP)

An [AgentRQ](https://agentrq.com) connector that proxies to Notion's official hosted
MCP server at `https://mcp.notion.com/mcp`. This is a remote connector — there is
no Rust code to build; the tools are provided by Notion's server. Authenticate with
your Notion account when prompted in the AgentRQ UI.

It is not in `connectors-manifest.json`, because the catalogue installs a listed
connector from its release asset and a remote server has no asset. To install it,
use its URL: `connectors install https://mcp.notion.com/mcp` (or **Discover ›
Remote MCP** in the web UI).

![icon](icon.svg)

## Tools

Tools are provided by the remote server. See [Notion's MCP documentation](https://developers.notion.com/docs/mcp)
for the available tools.

## Why remote?

Notion maintains an official MCP server. Rather than reimplementing the API in a
custom connector, this connector delegates to the official server, so it always
matches Notion's current capabilities.
