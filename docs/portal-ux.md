# App Depot Portal UX architecture

The Portal is the browser administration interface for the existing Server and API. It is a developer console for arbitrary software artifacts, not an app store or runtime manager. App names, release state, and visibility lead everyday screens; UUIDs, hashes, and internal identities remain available as secondary technical details. No installer, platform, executable format, declaration, or channel is mandatory.

## Workspaces and navigation

The Server workspace has Overview and Applications. Server Admin areas are Users & Access, Backups, Diagnostics, and Server Settings; they appear only as their screens are implemented. Account actions belong in the top-right user menu. There is no global Operations navigation item because the API has no operation list. Known operations remain accessible from the action that started them.

Selecting an App changes the sidebar to its Overview and Releases in M9.1. Channels, Declarations, and Settings join this context in later milestones. The current App name and a route back to Applications remain visible. Artifacts belong within their Release context. A Published Release can still be Private because App visibility is separate from Release state. Channels are optional and have no mandatory names.

Stable route conventions are `/` and `/apps` for the Server, `/apps/:appId` for an App, and child routes under `/apps/:appId` for App work. Authentication uses `/login` and `/setup` outside the workspace shell. M9.1 implements `/apps/new`, read-only `/apps/:appId/releases`, and `/apps/:appId/uploads/new` for a single file creating a Release in the selected App. The existing new-App-by-file flow remains at the transitional `/upload/new-app` route. Reserved routes for later work are `/apps/:appId/releases/new`, `/apps/:appId/releases/:releaseId`, `/apps/:appId/channels`, `/apps/:appId/declarations`, `/apps/:appId/settings`, `/admin/users`, `/admin/backups`, `/admin/diagnostics`, `/admin/settings`, `/operations/:id`, and `/account`; the Portal does not advertise them as working pages.

Declarative React Router provides nested routes, browser history, and direct reloads. The existing Server static-file fallback serves the Portal entry point on those routes. Page data is fetched through the API rather than stored in a global client-side catalog.

## Visual and interaction rules

Use neutral surfaces, one blue accent, semantic success/danger/neutral statuses with text as well as color, system fonts, and a spacing scale of 4, 8, 12, 16, 24, 32, and 48 px. Light and dark themes share semantic CSS tokens. Initial theme follows the OS; a local preference is available from the interface without changing Server settings. Tables prioritize human names and states, with technical values disclosed on demand. Each page has one clear primary action. Empty, loading, error, and permission states describe the actual outcome and next available action.

The application shell owns workspace navigation, context, theme access, and account actions. Page headers, actions, status badges, empty/loading states, problem notices, and technical details are reusable only where needed. A centralized API client handles same-origin cookies, CSRF, secure LAN-HTTP-compatible UUIDv4 idempotency keys, ETags, Problem Details, cancellation, and authorization errors. It does not replace Server authorization. An exact retry of a create request retains its idempotency key while its inputs remain unchanged. Publication and visibility are distinct decisions; an upload or registration never silently publishes.

The sidebar is persistent on desktop and a dismissible drawer below 900 px. Narrow forms use one column; tables retain readable primary fields and may scroll for technical data. Navigation, forms, and menus work with a keyboard, with visible focus and meaningful labels. Status changes use live text; reduced-motion preferences disable nonessential transitions.

## Current scope and future work

M9.1 provides session restoration, setup/login/logout, Server Overview, application search with server cursors, Draft App creation without an artifact, App Overview, read-only Release list, and file upload with Draft or Publish outcomes. Admin-only diagnostic aggregates may appear on Server Overview. The Portal does not infer release counts, activity feeds, storage history, per-App grants, or global operation lists from incomplete data. Later milestones can add Release details, richer upload flows, Artifacts, declarations, channels, users, backups, settings, and contextual operation views using the existing API. New backend capabilities require separate authorization and contract updates.
