import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";
import starlightLinksValidator from "starlight-links-validator";
import mermaid from "astro-mermaid";

export default defineConfig({
  site: "https://docs.sayiir.dev",
  integrations: [
    mermaid(),
    starlight({
      plugins: [starlightLinksValidator()],
      title: "Sayiir",
      logo: {
        src: "./public/favicon.png",
      },
      favicon: "/favicon.png",
      social: [
        { label: "GitHub", icon: "github", href: "https://github.com/sayiir/sayiir" },
        { label: "Discord", icon: "discord", href: "https://discord.gg/A2jWBFZsNK" },
      ],
      editLink: {
        baseUrl: "https://github.com/sayiir/sayiir/edit/main/website/",
      },
      head: [
        {
          tag: "meta",
          attrs: {
            name: "google-site-verification",
            content: "pz-_NhedmmAqWQ3AAia5X0iKopKJzwzunbrUbxrVMrU",
          },
        },
      ],
      customCss: ["./src/styles/custom.css"],
      sidebar: [
        {
          label: "Getting Started",
          items: [
            { label: "Python Quick Start", slug: "getting-started/python" },
            { label: "Node.js Quick Start", slug: "getting-started/nodejs" },
            { label: "Rust Quick Start", slug: "getting-started/rust" },
          ],
        },
        {
          label: "Concepts",
          items: [
            { label: "How Sayiir Works", slug: "concepts/how-it-works" },
            { label: "Architecture", slug: "concepts/architecture" },
          ],
        },
        {
          label: "Guides",
          items: [
            {
              label: "Durable Workflows",
              slug: "guides/durable-workflows",
            },
            {
              label: "Retries & Timeouts",
              slug: "guides/retries-and-timeouts",
            },
            {
              label: "Signals & Events",
              slug: "guides/signals-and-events",
            },
            {
              label: "Parallel Workflows",
              slug: "guides/parallel-workflows",
            },
            {
              label: "Loops & Iteration",
              slug: "guides/loops-and-iteration",
            },
            {
              label: "Composing Workflows",
              slug: "guides/composing-workflows",
            },
            {
              label: "Distributed Workers",
              slug: "guides/distributed-workers",
            },
            {
              label: "PostgreSQL in Production",
              slug: "guides/postgres-production",
            },
            {
              label: "Serialization & Migration",
              slug: "guides/serialization-and-versioning",
            },
          ],
        },
        {
          label: "Tutorials",
          items: [
            {
              label: "Order Processing (Python)",
              slug: "tutorials/order-processing-python",
            },
            {
              label: "Order Processing (Node.js)",
              slug: "tutorials/order-processing-nodejs",
            },
            {
              label: "Background Jobs (Rust)",
              slug: "tutorials/background-jobs-rust",
            },
            {
              label: "Approval Workflow (Signals)",
              slug: "tutorials/approval-workflow",
            },
            {
              label: "AI Research Agent (Python)",
              slug: "tutorials/ai-research-agent",
            },
          ],
        },
        {
          label: "API Reference",
          items: [
            { label: "Python API", slug: "reference/python-api" },
            { label: "Node.js API", slug: "reference/nodejs-api" },
            { label: "Rust API", slug: "reference/rust-api" },
            { label: "Configuration", slug: "reference/configuration" },
          ],
        },
        {
          label: "Comparisons",
          items: [
            { label: "Overview", slug: "comparisons/overview" },
            { label: "vs Temporal", slug: "comparisons/vs-temporal" },
            { label: "vs Airflow", slug: "comparisons/vs-airflow" },
            { label: "vs Prefect", slug: "comparisons/vs-prefect" },
            { label: "vs Step Functions", slug: "comparisons/vs-step-functions" },
            { label: "vs Elsa", slug: "comparisons/vs-elsa" },
          ],
        },
        {
          label: "Sayiir Server",
          items: [
            { label: "Platform Overview", slug: "server", badge: "Soon" },
          ],
        },
        { label: "Roadmap", slug: "roadmap" },
      ],
    }),
  ],
});
