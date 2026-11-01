// A minimal matter.js on/off light that serves as the first device under test of the interop
// harness. It is deliberately small: one endpoint with the On/Off cluster, commissioning
// parameters from the command line, state in the directory given by --storage-path.
//
// Usage (inside the container built from the Dockerfile next to this file):
//   node device.mjs --passcode=20202021 --discriminator=3840 --port=5540 --storage-path=/state
//
// matter.js reads `--key=value` arguments and `MATTER_*` environment variables into its
// environment variables, so `--log-format=plain` or `MATTER_LOG_FORMAT=plain` selects
// colour-free logs.

import { Endpoint, Environment, ServerNode, VendorId } from "@matter/main";
import { OnOffLightDevice } from "@matter/main/devices/on-off-light";

const vars = Environment.default.vars;

const passcode = vars.number("passcode") ?? 20202021;
const discriminator = vars.number("discriminator") ?? 3840;
const port = vars.number("port") ?? 5540;
const vendorId = vars.number("vendorid") ?? 0xfff1;
const productId = vars.number("productid") ?? 0x8000;

const node = await ServerNode.create({
    id: "interop-onoff-light",
    network: { port },
    commissioning: { passcode, discriminator },
    productDescription: {
        name: "Interop on/off light",
        deviceType: OnOffLightDevice.deviceType,
    },
    basicInformation: {
        vendorName: "matter-rs interop harness",
        vendorId: VendorId(vendorId),
        productName: "Interop on/off light",
        productLabel: "Interop on/off light",
        productId,
        nodeLabel: "Interop light",
        serialNumber: "interop-onoff-0001",
        uniqueId: "interop-onoff-light-0001",
    },
});

const light = new Endpoint(OnOffLightDevice, { id: "light" });
await node.add(light);

light.events.onOff.onOff$Changed.on(value => {
    console.log(`interop-dut: OnOff changed to ${value ? "on" : "off"}`);
});

for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => {
        console.log(`interop-dut: ${signal} received, going offline`);
        node.close()
            .then(() => process.exit(0))
            .catch(() => process.exit(1));
    });
}

await node.start();

// The harness waits for this line before it lets a controller commission the device.
console.log(`interop-dut: node is online, endpoint ${light.number} serves On/Off`);
