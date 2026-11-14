import React from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter, Route, Routes } from 'react-router'
import { AuthPage, AuthProvider, AppWorkspace, NotFound, RequireAdmin, RequireAuth, ServerWorkspace, ThemeProvider } from './portal'
import { ApplicationOverview, ApplicationReleases, ApplicationsPage, CreateApplicationPage, ServerOverview,
  UnavailablePage } from './pages'
import { AppDeclarations, AppSettings, ChannelsPage, NewRelease, OperationPage, ReleaseDetail } from './management'
import { UploadPage } from './uploads'
import { UsersPage, DiagnosticsPage, ServerSettingsPage } from './admin'
import { BackupsPage } from './backups'
import { AccountPage } from './account'
import { ArtworkPage } from './artwork'
import { ArtifactExplorer } from './explorer'
import './style.css'

createRoot(document.getElementById('root')!).render(<React.StrictMode><BrowserRouter><ThemeProvider><AuthProvider><Routes>
  <Route path="/login" element={<AuthPage setup={false} />} />
  <Route path="/setup" element={<AuthPage setup />} />
  <Route element={<RequireAuth><ServerWorkspace /></RequireAuth>}>
    <Route index element={<ServerOverview />} />
    <Route path="/apps" element={<ApplicationsPage />} />
    <Route path="/apps/new" element={<CreateApplicationPage />} />
    <Route path="/upload/new-app" element={<UploadPage target="new-app" />} />
    <Route path="/account" element={<AccountPage />} />
    <Route path="/operations/:id" element={<OperationPage />} />
    <Route element={<RequireAdmin />}>
      <Route path="/admin/users" element={<UsersPage />} />
      <Route path="/admin/backups" element={<BackupsPage />} />
      <Route path="/admin/diagnostics" element={<DiagnosticsPage />} />
      <Route path="/admin/settings" element={<ServerSettingsPage />} />
    </Route>
  </Route>
  <Route path="/apps/:appId" element={<RequireAuth><AppWorkspace /></RequireAuth>}>
    <Route index element={<ApplicationOverview />} />
    <Route path="releases" element={<ApplicationReleases />} />
    <Route path="releases/new" element={<NewRelease />} />
    <Route path="releases/:releaseId" element={<ReleaseDetail />} />
    <Route path="releases/:releaseId/artifacts/:associationId" element={<ArtifactExplorer />} />
    <Route path="releases/:releaseId/uploads/new" element={<UploadPage target="release" />} />
    <Route path="uploads/new" element={<UploadPage target="app" />} />
    <Route path="channels" element={<ChannelsPage />} />
    <Route path="artwork" element={<ArtworkPage />} />
    <Route path="declarations" element={<AppDeclarations />} />
    <Route path="settings" element={<AppSettings />} />
    <Route path="*" element={<UnavailablePage />} />
  </Route>
  <Route path="*" element={<RequireAuth><NotFound /></RequireAuth>} />
</Routes></AuthProvider></ThemeProvider></BrowserRouter></React.StrictMode>)
