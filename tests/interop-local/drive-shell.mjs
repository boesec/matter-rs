// Drives the matter.js shell (a Matter controller) through a list of steps: each step sends one
// shell command, waits for an expected output pattern and checks the output against an assertion.
//
// Usage: node drive-shell.mjs <steps.json>
//
// Environment:
//   MATTER_JS_APP    path of the shell, usually <matter.js>/packages/nodejs-shell/dist/esm/app.js
//   SHELL_STORAGE    directory for the controller's storage (kept between runs to stay commissioned)
//   SHELL_LOG        file that receives the complete shell output
//   PAIRING_CODE     substituted for ${PAIRING_CODE} in the commands
//
// A step is {"cmd", "expect", "assert"?, "reject"?, "timeoutMs"?}: `expect` is the regular
// expression that ends the step (an outcome, good or bad), `assert` must match the step's output
// for the step to pass (default: `expect`), `reject` must not match (default: "ERROR|Error
// happened"). The results are printed as JSON; the exit code is 1 when a step failed.

import { spawn } from "node:child_process";
import { createWriteStream, readFileSync } from "node:fs";

const stepsFile = process.argv[2];
if (!stepsFile) {
    console.error("usage: node drive-shell.mjs <steps.json>");
    process.exit(2);
}
const app = process.env.MATTER_JS_APP;
const storage = process.env.SHELL_STORAGE;
if (!app || !storage) {
    console.error("MATTER_JS_APP and SHELL_STORAGE must be set");
    process.exit(2);
}
const pairingCode = process.env.PAIRING_CODE ?? "";
const steps = JSON.parse(readFileSync(stepsFile, "utf8"));
const log = createWriteStream(process.env.SHELL_LOG ?? "shell.log", { flags: "a" });

const env = {
    ...process.env,
    MATTER_STORAGE_PATH: storage,
    MATTER_LOG_LEVEL: process.env.MATTER_LOG_LEVEL ?? "info",
};
const child = spawn(process.execPath, [app, "1"], { env, stdio: ["pipe", "pipe", "pipe"] });

let buffer = "";
let waiter = null;
const onData = chunk => {
    const text = chunk.toString();
    log.write(text);
    buffer += text;
    if (waiter && waiter.re.test(buffer)) {
        const current = waiter;
        waiter = null;
        clearTimeout(current.timer);
        current.resolve(true);
    }
};
child.stdout.on("data", onData);
child.stderr.on("data", onData);
child.on("exit", code => log.write(`\n[driver] shell exited with ${code}\n`));

function waitFor(re, ms) {
    return new Promise(resolve => {
        if (re.test(buffer)) {
            resolve(true);
            return;
        }
        const timer = setTimeout(() => {
            waiter = null;
            resolve(false);
        }, ms);
        waiter = { re, resolve, timer };
    });
}

const results = [];
let failed = false;
await waitFor(/matter-node>|>\s*$/m, 30000);
for (const step of steps) {
    const cmd = step.cmd.replaceAll("${PAIRING_CODE}", pairingCode);
    buffer = "";
    log.write(`\n[driver] >>> ${cmd.replaceAll(pairingCode || "\u0000", "<pairing code>")}\n`);
    const started = Date.now();
    child.stdin.write(cmd + "\n");
    const matched = await waitFor(new RegExp(step.expect, "m"), step.timeoutMs ?? 60000);
    // Let trailing output of the step arrive before judging it.
    await new Promise(resolve => setTimeout(resolve, 300));
    const output = buffer;
    const assertion = new RegExp(step.assert ?? step.expect, "m");
    const rejection = new RegExp(step.reject ?? "ERROR|Error happened", "m");
    const ok = matched && assertion.test(output) && !rejection.test(output);
    const result = {
        cmd: cmd.replaceAll(pairingCode || "\u0000", "<pairing code>"),
        ok,
        matched,
        ms: Date.now() - started,
    };
    if (!ok) {
        failed = true;
        result.output = output.split("\n").filter(line => line.trim() !== "").slice(-12);
    }
    results.push(result);
    log.write(`\n[driver] <<< ${ok ? "PASS" : "FAIL"} after ${result.ms} ms\n`);
}
console.log(JSON.stringify(results, null, 2));
child.stdin.write("exit\n");
setTimeout(() => {
    child.kill();
    log.end();
    process.exit(failed ? 1 : 0);
}, 3000);
