import { isValidSonioxEndpointDelay } from './sonioxSettings'

export function checkSonioxEndpointDelay() {
  for (const value of [null, 500, 2000, 3000]) {
    if (!isValidSonioxEndpointDelay(value)) throw new Error(`Rejected valid Soniox delay: ${value}`)
  }
  for (const value of [0, 499, 3001, 10000, 120000, 500.5, NaN, Infinity]) {
    if (isValidSonioxEndpointDelay(value)) throw new Error(`Accepted invalid Soniox delay: ${value}`)
  }
  return 12
}
