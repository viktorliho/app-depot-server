import { sha256 } from '@noble/hashes/sha256'
import { bytesToHex } from '@noble/hashes/utils'

export type User = { id: string; username: string; serverAdmin: boolean; publishPermission: boolean }
export type BrowserSession = { user: User; csrfToken: string }
export type DepotApp = { id: string; name: string; slug: string; ownerId: string; state: 'Draft' | 'Published'; visibility: 'Private' | 'Public' }
export type AppPage = { items: DepotApp[]; nextCursor: string | null }
export type Release = { id: string; appId: string; state: 'Draft' | 'Published'; version?: string | null }
export type ReleasePage = { items: Release[]; nextCursor: string | null }
export type Page<T> = { items: T[]; nextCursor: string | null }
export type Artifact = { id: string; kind: 'File' | 'Tree'; size?: number | null }
export type Association = { id: string; releaseId: string; artifactId: string }
export type Channel = { appId: string; name: string; releaseId: string }
export type ChannelMove = { id: string; appId: string; name: string; releaseId: string; movedAt: string }
export type Declaration = { revisionId: string; data: unknown }
export type DeclarationRevision = { id: string; createdAt: string; data: unknown }
export type DeclarationSnapshot = { releaseId: string; app: Declaration; release: Declaration; associations: { associationId: string; declaration: Declaration }[] }
export type InventoryEntry = { type: 'File' | 'Directory'; path: string; fileId?: string | null; size?: number | null; sha256?: string | null; executable?: boolean | null; transferRequired?: boolean }
export type UploadProgress = { ranges: { start: number; end: number }[]; nextCursor: string | null; progressVersion: number }
export type Operation = { id: string; kind: 'Finalize' | 'CancelUpload' | 'DeleteApp' | 'DeleteRelease' | 'DeleteAssociation' | 'Backup'; status: 'Accepted' | 'Running' | 'Completed' | 'Failed'; backupId?: string; backupOutputAvailable?: boolean; accessRevoked?: boolean; physicalCleanup?: string; result?: FinalizationResult | null; problem?: { title: string; detail?: string } | null }
export type Diagnostics = { readiness: string; recovery: string; migration: string; storagePressure: string; windowSeconds: number; requestFailures: number; requestLatencyMsP95: number; activeUploads: number; activeTransfers: number; pendingCleanup: number; retryableCleanup: number; backupsRunning: number; backupsFailed: number }
export type ServerSettings = { anonymousReadEnabled: boolean }
export type TokenMetadata = { active: boolean; issuedAt: string | null }
export type TokenIssue = { token: string; metadata: TokenMetadata }
export type BackupSummary = { id: string; formatVersion: number; capturedAt: string; completedAt: string; memberCount: number; totalBytes: number }
export type BackupMember = { entryId: string; kind: 'SqliteSnapshot' | 'CasObject'; relativePath: string; size: number; sha256: string }
export type BackupManifest = { formatVersion: number; backupId: string; capturedAt: string; catalogFormatVersion: number; members: BackupMember[] }
export type UploadSession = { id: string; operationId: string; file?: { fileId: string } | null; phase: string; inventoryComplete: boolean }
export type FinalizationResult = { phase: string; appId?: string; releaseId?: string; artifactId?: string; associationId?: string; problem?: { title: string } }

export class ApiError extends Error {
  constructor(public status: number, public code: string, title: string, public detail?: string) {
    super(title)
  }
}

export function errorMessage(error: unknown): string {
  if (!(error instanceof ApiError)) return error instanceof Error ? error.message : 'Unexpected error.'
  if (error.status === 0) return 'Cannot reach the server. Check the connection and try again.'
  if (error.status === 401) return 'Your session has expired. Sign in again.'
  if (error.status === 403) return 'You do not have permission for this action.'
  if (error.status === 404) return 'This item is unavailable or you do not have access to it.'
  if (error.status === 409) return error.detail || error.message || 'This action conflicts with the current state.'
  if (error.status === 412) return 'This item changed. Refresh it before trying again.'
  if (error.status === 428) return 'A current version is required. Refresh and try again.'
  if (error.status === 503) return 'The server is not ready. Try again after recovery completes.'
  return error.detail || error.message || `Request failed (${error.status}).`
}

export function idempotencyKey(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  const hex = bytesToHex(bytes)
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

export async function fileSha256(file: File): Promise<string> {
  const hash = sha256.create()
  for (let offset = 0; offset < file.size; offset += 4 * 1024 * 1024) {
    hash.update(new Uint8Array(await file.slice(offset, offset + 4 * 1024 * 1024).arrayBuffer()))
  }
  return bytesToHex(hash.digest())
}

type Options = RequestInit & { csrf?: string; idempotencyKey?: string; ifMatch?: string; authenticated?: boolean }

export async function apiRequest<T>(path: string, options: Options = {}): Promise<{ value: T; response: Response }> {
  const { csrf, idempotencyKey: key, ifMatch, authenticated, headers, ...init } = options
  const requestHeaders = new Headers(headers)
  if (csrf) requestHeaders.set('X-CSRF-Token', csrf)
  if (key) requestHeaders.set('Idempotency-Key', key)
  if (ifMatch) requestHeaders.set('If-Match', ifMatch)
  let response: Response
  try {
    response = await fetch(`/api/v1${path}`, { ...init, credentials: 'same-origin', headers: requestHeaders })
  } catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    throw new ApiError(0, 'network_error', 'Network error')
  }
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { code?: string; title?: string; detail?: string }
    if (response.status === 401 && authenticated) window.dispatchEvent(new Event('app-depot-unauthorized'))
    if (response.status === 403 && authenticated && /^\/(admin|users|settings|backups)(\/|$)/.test(path)) window.dispatchEvent(new Event('app-depot-permissions-changed'))
    throw new ApiError(response.status, body.code || 'request_failed', body.title || `Request failed (${response.status})`, body.detail)
  }
  const value = response.status === 204 ? undefined as T : await response.json() as T
  return { value, response }
}

export async function apiStream(path: string, signal?: AbortSignal): Promise<Response> {
  let response: Response
  try { response = await fetch(`/api/v1${path}`, { credentials: 'same-origin', cache: 'no-store', signal }) }
  catch (error) {
    if (error instanceof DOMException && error.name === 'AbortError') throw error
    throw new ApiError(0, 'network_error', 'Network error')
  }
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { code?: string; title?: string; detail?: string }
    if (response.status === 401) window.dispatchEvent(new Event('app-depot-unauthorized'))
    throw new ApiError(response.status, body.code || 'request_failed', body.title || `Request failed (${response.status})`, body.detail)
  }
  return response
}

export async function resource<T>(path: string, signal?: AbortSignal): Promise<{ value: T; etag: string }> {
  const result = await apiRequest<T>(path, { authenticated: true, signal })
  const etag = result.response.headers.get('ETag')
  if (!etag || etag.startsWith('W/')) throw new Error('The Server did not provide a strong version validator.')
  return { value: result.value, etag }
}

export function jsonBody(value: unknown): Pick<Options, 'headers' | 'body'> {
  return { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value) }
}
