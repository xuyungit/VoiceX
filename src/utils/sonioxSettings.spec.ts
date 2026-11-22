import { isValidSonioxEndpointDelay } from './sonioxSettings'

export function checkSonioxEndpointDelay() {
  for (const value of [null, 500, 3000]) {
    if (!isValidSonioxEndpointDelay(value)) throw new Error(`Rejected valid Soniox delay: ${value}`)
  }
  for (const value of [499, 3001, 500.5, NaN]) {
    if (isValidSonioxEndpointDelay(value)) throw new Error(`Accepted invalid Soniox delay: ${value}`)
  }
}
