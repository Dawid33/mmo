{{- define "thanos.labels" -}}
app.kubernetes.io/name: thanos
app.kubernetes.io/part-of: monitoring
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "thanos.objstoreVolume" -}}
- name: objstore
  secret:
    secretName: {{ .Values.objstoreSecret }}
{{- end }}

{{- define "thanos.objstoreMount" -}}
- name: objstore
  mountPath: /etc/thanos
  readOnly: true
{{- end }}

{{- define "thanos.probes" -}}
livenessProbe:
  httpGet: { path: /-/healthy, port: http }
  periodSeconds: 30
  failureThreshold: 4
readinessProbe:
  httpGet: { path: /-/ready, port: http }
  periodSeconds: 10
  failureThreshold: 20
{{- end }}

{{- define "thanos.securityContext" -}}
securityContext:
  runAsNonRoot: true
  runAsUser: 1001
  runAsGroup: 1001
  fsGroup: 1001
  seccompProfile: { type: RuntimeDefault }
{{- end }}
