// Soniox WebSocket API: https://soniox.com/docs/api-reference/stt/websocket-api
export function isValidSonioxEndpointDelay(value: number | null): boolean {
  return value === null || (Number.isInteger(value) && value >= 500 && value <= 3000)
}
