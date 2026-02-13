# Use Cases

Every enterprise runs on workflows — whether they call them that or not. Order processing, user onboarding, payment reconciliation, data pipelines, compliance checks. These are all multi-step processes that need to be reliable, observable, and recoverable when things go wrong.

Sayiir is built for teams that need these guarantees without the complexity of traditional workflow engines.

---

## Fintech & Payments

- Payment processing pipelines with retry and reconciliation
- KYC/AML verification workflows
- Transaction monitoring and fraud detection
- Multi-step onboarding flows

## E-commerce & Marketplaces

- Order fulfillment orchestration
- Inventory synchronization across channels
- Refund and return processing
- Seller payout workflows

## SaaS & B2B

- User provisioning and deprovisioning
- Subscription lifecycle management
- Data pipeline orchestration
- Multi-tenant job scheduling

## Healthcare & Compliance

- Patient data processing with audit trails
- Insurance claim workflows
- Regulatory reporting pipelines
- Document processing and approvals

## Data & ML Pipelines

- ETL/ELT workflow orchestration
- ML training pipelines (prep → train → evaluate → deploy)
- Batch inference scheduling
- Feature engineering workflows
- Data quality and validation checks

## AI Agents

- Multi-step agent orchestration with checkpointing
- Tool call durability — resume after failures without re-running LLM calls
- Human-in-the-loop approval chains
- Long-running agent loops (hours/days) with crash recovery
- No determinism constraints — LLM calls are inherently non-deterministic, and that's fine

## Infrastructure & DevOps

- CI/CD pipeline orchestration
- Infrastructure provisioning workflows
- Incident response automation
- Scheduled maintenance tasks

---

## Deployment

### Open Source

| Platform           | Status      | Notes                     |
| ------------------ | ----------- | ------------------------- |
| Bare metal / VMs   | Ready       | Any Linux/macOS/Windows   |
| Kubernetes         | Ready       | StatefulSet or Deployment |
| AWS ECS / Fargate  | Ready       | Container-based           |
| AWS Lambda         | Ready       | With external persistence |
| Cloudflare Workers | In Progress | Via Durable Objects       |

### Enterprise (Planned)

For teams that need more:

- **Managed Control Plane** — Scalable gRPC server on Kubernetes
- **Web UI** — Workflow visualization, debugging, manual interventions
- **Audit Logging** — Complete execution history for compliance
- **Time-Critical Tasks** — Hard deadline enforcement with SLA guarantees, automatic escalation on breach
- **Worker Pools** — Isolated execution environments per tenant/workload
- **Code Sandboxing** — Secure execution of untrusted or tenant-provided code with resource limits and isolation
- **Auto-scaling** — Dynamic worker provisioning based on queue depth
- **Security** — mTLS worker authentication, RBAC, payload-level encryption, snapshot integrity verification (HMAC), secure credential passing (Vault, AWS Secrets Manager)
