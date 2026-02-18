/**
 * Order Processing Pipeline — demonstrates fork/join, signals, and webhooks.
 *
 * Workflow:
 *   validate → (charge || inventory) → join → wait for shipping webhook → confirm
 *
 * Run:
 *   pnpm start
 *
 * Then simulate a shipping provider webhook:
 *   curl -X POST http://localhost:3000/webhooks/shipping \
 *     -H "Content-Type: application/json" \
 *     -d '{"orderId": "order-1", "trackingNumber": "1Z999AA10123456784", "carrier": "ups"}'
 */

import {
  task,
  flow,
  branch,
  runDurableWorkflow,
  resumeWorkflow,
  sendSignal,
  InMemoryBackend,
} from "sayiir";
import { createServer } from "node:http";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface Order {
  orderId: string;
  customerEmail: string;
  amount: number;
}

interface ShipmentEvent {
  trackingNumber: string;
  carrier: string;
}

// ---------------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------------

const validateOrder = task("validate-order", (order: Order) => {
  if (order.amount <= 0) throw new Error("Invalid amount");
  if (!order.customerEmail) throw new Error("Missing customer email");
  return { ...order, validated: true as const };
});

const chargePayment = task(
  "charge-payment",
  (order: Order) => {
    // In production: call Stripe, Square, etc.
    return { ...order, paymentId: `pay_${order.orderId}`, charged: true as const };
  },
  { timeout: "30s", retry: { maxAttempts: 3, initialDelay: "1s", backoffMultiplier: 2.0 } },
);

const checkInventory = task("check-inventory", (order: Order) => {
  // In production: query inventory service
  return { ...order, inStock: true as const };
});

const sendConfirmation = task(
  "send-confirmation",
  (shipment: ShipmentEvent) => {
    // In production: send email via SendGrid, Postmark, etc.
    return `Order shipped via ${shipment.carrier}, tracking: ${shipment.trackingNumber}`;
  },
);

// ---------------------------------------------------------------------------
// Workflow definition
// ---------------------------------------------------------------------------

const workflow = flow<Order>("order-processing")
  .then(validateOrder)
  .fork([
    branch("payment", chargePayment),
    branch("inventory", checkInventory),
  ])
  .join("finalize", ([payment, inventory]) => ({
    orderId: payment.orderId,
    customerEmail: payment.customerEmail,
    paymentId: payment.paymentId,
    inStock: inventory.inStock,
  }))
  // Park here until the shipping provider calls our webhook
  .waitForSignal<ShipmentEvent>("shipping-wait", "shipment_dispatched", { timeout: "72h" })
  .then(sendConfirmation)
  .build();

// ---------------------------------------------------------------------------
// Backend — swap to PostgresBackend.connect(url) for production
// ---------------------------------------------------------------------------

const backend = new InMemoryBackend();

// ---------------------------------------------------------------------------
// Submit a test order
// ---------------------------------------------------------------------------

const order: Order = {
  orderId: "order-1",
  customerEmail: "alice@example.com",
  amount: 99.99,
};

const status = runDurableWorkflow(workflow, order.orderId, order, backend);
console.log(`Order submitted — status: ${status.status}`);
// "awaiting_signal" — waiting for shipment webhook

// ---------------------------------------------------------------------------
// Webhook server — receives shipping provider callbacks
// ---------------------------------------------------------------------------

const server = createServer(async (req, res) => {
  if (req.method === "POST" && req.url === "/webhooks/shipping") {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk as Buffer);
    const body = JSON.parse(Buffer.concat(chunks).toString());

    const instanceId = body.orderId as string;
    const shipment: ShipmentEvent = {
      trackingNumber: body.trackingNumber,
      carrier: body.carrier,
    };

    // Deliver the webhook payload as a signal to the parked workflow
    sendSignal(instanceId, "shipment_dispatched", shipment, backend);
    const result = resumeWorkflow(workflow, instanceId, backend);

    console.log(`Webhook received for ${instanceId} — workflow resumed`);
    if (result.status === "completed") {
      console.log(`Result: ${result.output}`);
    }

    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ status: "ok", workflowStatus: result.status }));

    if (result.status === "completed") {
      server.close();
    }
  } else {
    res.writeHead(404);
    res.end("Not found");
  }
});

const PORT = 3000;
server.listen(PORT, () => {
  console.log(`\nWebhook server listening on http://localhost:${PORT}`);
  console.log(`\nSimulate a shipping webhook:`);
  console.log(`  curl -X POST http://localhost:${PORT}/webhooks/shipping \\`);
  console.log(`    -H "Content-Type: application/json" \\`);
  console.log(`    -d '{"orderId": "order-1", "trackingNumber": "1Z999AA10123456784", "carrier": "ups"}'`);
});
