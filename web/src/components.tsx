import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { errorMessage } from './api'

export function PageHeader({ title, description, actions }: { title: string; description?: string; actions?: ReactNode }) {
  return <div className="page-header"><div><h1>{title}</h1>{description && <p>{description}</p>}</div>{actions && <div className="page-actions">{actions}</div>}</div>
}

export function ActionLink({ to, children, secondary = false }: { to: string; children: ReactNode; secondary?: boolean }) {
  return <Link className={`button ${secondary ? 'button-secondary' : 'button-primary'}`} to={to}>{children}</Link>
}

export function StatusBadge({ value }: { value: string }) {
  const tone = value === 'Published' || value === 'Ready' || value === 'Public' || value === 'Normal' || value === 'Completed' ? 'success'
    : value === 'Failed' || value === 'NotReady' || value === 'Critical' ? 'danger' : value === 'Warning' ? 'warning' : 'neutral'
  return <span className={`status status-${tone}`}><span aria-hidden="true" className="status-dot" />{value}</span>
}

export function EmptyState({ title, detail, action }: { title: string; detail: string; action?: ReactNode }) {
  return <div className="empty-state"><h2>{title}</h2><p>{detail}</p>{action}</div>
}

export function LoadingState({ label = 'Loading…' }: { label?: string }) {
  return <div className="loading-state" role="status">{label}</div>
}

export function ProblemNotice({ error, retry }: { error: unknown; retry?: () => void }) {
  return <div className="problem-notice" role="alert"><strong>Could not complete this request</strong><p>{errorMessage(error)}</p>{retry && <button className="button button-secondary" onClick={retry}>Try again</button>}</div>
}

export function TechnicalDetails({ children }: { children: ReactNode }) {
  return <details className="technical-details"><summary>Technical details</summary><div>{children}</div></details>
}
