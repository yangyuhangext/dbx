import test from "node:test";
import assert from "node:assert/strict";
import { hasEnabledTransportLayers } from "../../apps/desktop/src/lib/connectionTransport.ts";

test("hasEnabledTransportLayers matches effective transport layer visibility", () => {
  assert.equal(hasEnabledTransportLayers(undefined), false);
  assert.equal(hasEnabledTransportLayers({ transport_layers: [] }), false);
  assert.equal(
    hasEnabledTransportLayers({
      transport_layers: [{ type: "proxy", id: "proxy", enabled: false, host: "127.0.0.1", port: 1080 }],
    }),
    false,
  );
  assert.equal(
    hasEnabledTransportLayers({
      transport_layers: [{ type: "proxy", id: "proxy", host: "127.0.0.1", port: 1080 }],
    }),
    true,
  );
});
