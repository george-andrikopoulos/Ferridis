// Ferridis VS Code extension v0.2 — entry point.
//
// On activation:
//   1. Read ferridis.serverPath / ferridis.adaptersConfig / ferridis.keychainNamespace.
//   2. Write (or remove) the ferridis-mcp-server entry in mcp.servers so VS Code's
//      built-in MCP machinery starts it and discovers all tools dynamically.
//   3. Re-sync whenever the Ferridis settings change.
//
// Tools come from the running ferridis-mcp-server, which wraps native Ferridis
// adapters plus any MCP stdio/SSE servers listed in the adapters config.

import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";

const MCP_SERVER_KEY = "ferridis";

export async function activate(
    context: vscode.ExtensionContext,
): Promise<void> {
    const output = vscode.window.createOutputChannel("Ferridis");
    context.subscriptions.push(output);
    output.appendLine(`[ferridis] activate (vscode=${vscode.version})`);

    await syncMcpServer(output);

    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration(async (e) => {
            if (e.affectsConfiguration("ferridis")) {
                output.appendLine("[ferridis] settings changed; re-syncing");
                await syncMcpServer(output);
            }
        }),
        vscode.commands.registerCommand("ferridis.configure", () => {
            vscode.commands.executeCommand(
                "workbench.action.openSettings",
                "ferridis",
            );
        }),
        vscode.commands.registerCommand("ferridis.showLogs", () => {
            output.show(true);
        }),
    );
}

export function deactivate(): void {}

async function syncMcpServer(output: vscode.OutputChannel): Promise<void> {
    const cfg = vscode.workspace.getConfiguration("ferridis");
    const serverPath = cfg.get<string>("serverPath", "").trim();

    const mcpCfg = vscode.workspace.getConfiguration("mcp");
    const servers = mcpCfg.get<Record<string, unknown>>("servers") ?? {};

    if (!serverPath) {
        if (MCP_SERVER_KEY in servers) {
            const updated = { ...servers };
            delete updated[MCP_SERVER_KEY];
            await mcpCfg.update(
                "servers",
                updated,
                vscode.ConfigurationTarget.Global,
            );
            output.appendLine(
                "[ferridis] removed from mcp.servers (ferridis.serverPath is unset)",
            );
        }
        void vscode.window
            .showWarningMessage(
                "Ferridis: set ferridis.serverPath to the ferridis-mcp-server binary to enable tool discovery.",
                "Open Settings",
            )
            .then((action) => {
                if (action === "Open Settings") {
                    vscode.commands.executeCommand(
                        "workbench.action.openSettings",
                        "ferridis.serverPath",
                    );
                }
            });
        return;
    }

    const adaptersConfig =
        cfg.get<string>("adaptersConfig", "").trim() ||
        path.join(os.homedir(), ".ferridis", "adapters.json");

    const keychainNamespace = cfg.get<string>("keychainNamespace", "ferridis");

    const updated = {
        ...servers,
        [MCP_SERVER_KEY]: {
            type: "stdio",
            command: serverPath,
            args: [
                "--adapters-config",
                adaptersConfig,
                "--keychain-namespace",
                keychainNamespace,
            ],
        },
    };

    await mcpCfg.update("servers", updated, vscode.ConfigurationTarget.Global);
    output.appendLine(
        `[ferridis] registered in mcp.servers: ${serverPath} --adapters-config ${adaptersConfig}`,
    );
}
