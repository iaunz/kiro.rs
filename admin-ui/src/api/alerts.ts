import axios from 'axios'
import { storage } from '@/lib/storage'
import type {
  AlertConfigResponse,
  UpdateAlertConfigRequest,
  AlertChannelRequest,
  AlertStatusResponse,
} from '@/types/api'

const api = axios.create({
  baseURL: '/api/admin',
  headers: { 'Content-Type': 'application/json' },
})

api.interceptors.request.use((config) => {
  const apiKey = storage.getApiKey()
  if (apiKey) {
    config.headers['x-api-key'] = apiKey
  }
  return config
})

export async function getAlertConfig(): Promise<AlertConfigResponse> {
  const { data } = await api.get<AlertConfigResponse>('/alerts/config')
  return data
}

export async function updateAlertConfig(
  req: UpdateAlertConfigRequest
): Promise<AlertConfigResponse> {
  const { data } = await api.put<AlertConfigResponse>('/alerts/config', req)
  return data
}

export async function getAlertStatus(): Promise<AlertStatusResponse> {
  const { data } = await api.get<AlertStatusResponse>('/alerts/status')
  return data
}

export async function createAlertChannel(
  req: AlertChannelRequest
): Promise<AlertConfigResponse> {
  const { data } = await api.post<AlertConfigResponse>('/alerts/channels', req)
  return data
}

export async function updateAlertChannel(
  id: string,
  req: AlertChannelRequest
): Promise<AlertConfigResponse> {
  const { data } = await api.put<AlertConfigResponse>(`/alerts/channels/${id}`, req)
  return data
}

export async function deleteAlertChannel(id: string): Promise<AlertConfigResponse> {
  const { data } = await api.delete<AlertConfigResponse>(`/alerts/channels/${id}`)
  return data
}
