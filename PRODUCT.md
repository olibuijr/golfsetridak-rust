# Product

## Register

product

## Users

Golfsetrið Akureyri staff and owners — managing bookings, users, products, tournaments, gift cards, and payments for an Icelandic golf center. They use the admin panel on desktop during business hours; occasional mobile use for quick checks.

## Product Purpose

A single-binary Rust web application replacing the old Next.js site. Serves the public-facing golf center website (booking, shop, gift cards) and an authenticated admin dashboard for day-to-day operations management.

## Brand Personality

Clean, functional, Icelandic, grounded. The dark emerald theme reflects the golf course setting. Professional but warm — not corporate, not playful. The admin panel should feel like a serious tool, not a toy.

## Anti-references

- Flashy SaaS dashboards with excessive gradients and decorative motion
- Bloated admin panels with nested cards and too many layers
- Light-mode-only tools (this is a dark-themed surface)

## Design Principles

1. **Tool-first, not show-first** — the admin panel is a daily work instrument; every pixel serves a task
2. **Icelandic clarity** — labels, navigation, and messaging use natural Icelandic, not translated jargon
3. **Density with breath** — compact enough for data work, spaced enough to avoid fatigue
4. **Consistent vocabulary** — same visual language across all admin screens; no surprises between pages
5. **Responsive without compromise** — sidebar collapses, tables scroll, but functionality never hides

## Accessibility & Inclusion

- WCAG 2.2 AA target
- Dark theme with sufficient contrast
- Keyboard-navigable admin flows
- Respects `prefers-reduced-motion`
