---
name: rex-design
description: Design and improve user interfaces, including landing pages, dashboards, tables, forms, and multi-step flows. Use for UI design, redesign, implementation, and visual review.
---

# REX Design

Design for the task people need to finish, not a screenshot. A polished page that hides the next action or breaks at narrow widths is unfinished. Start from the existing product and code, not a generic visual trend.

## Brief and audit

Identify the audience, primary task, constraints, device sizes, and success measure. Mark assumptions when the brief is thin; ask if an assumption changes the product's purpose or audience. Inspect existing routes, components, tokens, assets, and conventions. For redesigns, capture the current UI and name concrete failures in hierarchy, navigation, readability, responsiveness, accessibility, or task completion. Preserve working content and flows. Never invent testimonials, metrics, logos, prices, or product claims.

## Interaction before styling

Map entry, decisions, primary action, outcomes, and recovery. Cover loading, empty, error, success, disabled, and long-content states where relevant. Dashboards need legible status, freshness, filters, and next actions. Tables need scannability and useful empty states. Forms need clear requirements, errors, and input preservation. Multi-step flows need progress and a way back.

Choose a visual direction tied to audience and product. Define a compact system for type, spacing, colors, surfaces, borders, and interaction states. Rank the primary action clearly. Use contrast, alignment, content order, and whitespace before decoration. Do not default to gradients, glass, oversized hero copy, generic AI imagery, or animation instead of hierarchy. Motion should explain state or direction and respect reduced-motion preferences. A reference library is evidence to compare, not a palette of required effects. Choose patterns for the brief and explain what you rejected; do not assume dark glass, bento grids, asymmetry, or hand-drawn textures are inherently better. Keep asset origin, license, responsive behavior, fallback, and performance cost with any adopted reference.

## State coverage ledger

Before coding, make a short ledger with one row per consequential state. Use these columns: user intent, entry point, data condition, visible feedback, next action, narrow-screen behavior, and proof. At minimum cover first use, normal use, no results, invalid input, failure, and a long or crowded case where they apply. Mark a state not applicable only with a reason. This is a planning aid, not a checklist that substitutes for examining the product.

For each row, ask whether the user can tell what happened and recover without losing their work. Prioritize a broken task or inaccessible control over decoration. If the task is only a visual critique, use the ledger to name missing states instead of pretending they were implemented. When handing off code, distinguish states actually exercised in the rendered UI from those inferred from source or left untested.

## Design decisions under pressure

Do not settle on the first attractive direction. For a consequential page or flow, sketch two meaningfully different ways to organize the task, not two color schemes. Compare them against the brief: Can a new user locate the primary action? Can a returning user see what changed? Can someone recover from a mistake? Which version handles dense data and a narrow screen without hiding essential controls? Choose one and record the tradeoff that made it win. If the brief does not support a confident choice, show the decision to the product owner rather than inventing a user preference.

Before final polish, run an adversarial pass on the chosen direction. Try a long name, sparse data, a permission error, a delayed response, keyboard-only navigation, zoomed text, and a narrow viewport. Keep the result tied to evidence: what was rendered and exercised, what failed, what changed, and what remains an assumption. Do not label a design superior to another product or skill without a shared task, comparable criteria, and actual results.

## Implement and verify

Reuse components and tokens where possible. Use semantic elements, real labels, keyboard access, visible focus, sufficient contrast, sensible tab order, and usable touch targets. Check narrow, medium, and wide layouts instead of shrinking desktop. Handle long labels, zero results, and realistic data density; preserve routing and state behavior.

Run relevant checks, then render the actual UI. Inspect pixels at narrow and wide widths. Exercise the primary path with keyboard and pointer, plus an error or empty state and long content. Fix issues and inspect the changed result. Source code, DOM, or a successful build alone cannot prove visual quality. Source-pattern and symmetry scores can catch known mistakes but do not measure task fit, accessibility, or visual quality; never use them as a pass certificate or optimize a page merely to raise a score. If rendering is unavailable, explicitly mark the visual work unverified.

## Handoff

State the design direction and why it fits; changed files; paths and states tested; assumptions and remaining risks. Make critique specific: the issue, its effect on the task, and the smallest useful fix.
