import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  getAlertConfig,
  updateAlertConfig,
  getAlertStatus,
  createAlertChannel,
  updateAlertChannel,
  deleteAlertChannel,
} from '@/api/alerts'
import type { AlertChannelRequest, UpdateAlertConfigRequest } from '@/types/api'

export function useAlertConfig() {
  return useQuery({ queryKey: ['alert-config'], queryFn: getAlertConfig })
}

export function useAlertStatus() {
  return useQuery({
    queryKey: ['alert-status'],
    queryFn: getAlertStatus,
    refetchInterval: 60000,
  })
}

export function useUpdateAlertConfig() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: UpdateAlertConfigRequest) => updateAlertConfig(req),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}

export function useCreateAlertChannel() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (req: AlertChannelRequest) => createAlertChannel(req),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}

export function useUpdateAlertChannel() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ id, req }: { id: string; req: AlertChannelRequest }) =>
      updateAlertChannel(id, req),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}

export function useDeleteAlertChannel() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => deleteAlertChannel(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['alert-config'] }),
  })
}
