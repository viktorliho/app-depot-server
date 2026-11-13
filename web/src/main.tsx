import React from 'react'
import { createRoot } from 'react-dom/client'
import { BrowserRouter, Route, Routes } from 'react-router'
import { AuthPage, AuthProvider, AppWorkspace, NotFound, RequireAuth, ServerWorkspace, ThemeProvider } from './portal'
import { ApplicationOverview, ApplicationReleases, ApplicationsPage, CreateApplicationPage, ServerOverview,
  UnavailablePage } from './pages'
import { AppDeclarations, AppSettings, ChannelsPage, NewRelease, ReleaseDetail } from './management'
import { UploadPage } from './uploads'
import './style.css'

createRoot(document.getElementById('root')!).render(<React.StrictMode><BrowserRouter><ThemeProvider><AuthProvider><Routes>
  <Route path="/login" element={<AuthPage setup={false} />} />
  <Route path="/setup" element={<AuthPage setup />} />
  <Route element={<RequireAuth><ServerWorkspace /></RequireAuth>}>
    <Route index element={<ServerOverview />} />
    <Route path="/apps" element={<ApplicationsPage />} />
    <Route path="/apps/new" element={<CreateApplicationPage />} />
    <Route path="/upload/new-app" element={<UploadPage target="new-app" />} />
  </Route>
  <Route path="/apps/:appId" element={<RequireAuth><AppWorkspace /></RequireAuth>}>
    <Route index element={<ApplicationOverview />} />
    <Route path="releases" element={<ApplicationReleases />} />
    <Route path="releases/new" element={<NewRelease />} />
    <Route path="releases/:releaseId" element={<ReleaseDetail />} />
    <Route path="releases/:releaseId/uploads/new" element={<UploadPage target="release" />} />
    <Route path="uploads/new" element={<UploadPage target="app" />} />
    <Route path="channels" element={<ChannelsPage />} />
    <Route path="declarations" element={<AppDeclarations />} />
    <Route path="settings" element={<AppSettings />} />
    <Route path="*" element={<UnavailablePage />} />
  </Route>
  <Route path="*" element={<RequireAuth><NotFound /></RequireAuth>} />
</Routes></AuthProvider></ThemeProvider></BrowserRouter></React.StrictMode>)
