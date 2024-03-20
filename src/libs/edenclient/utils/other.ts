// source https://github.com/elysiajs/eden/blob/main/src/treaty/utils.ts
/**
 * Utility function for composing a URL path with optional query parameters.
 * @param domain The base domain for the API.
 * @param path The path segment for the API endpoint.
 * @param query Optional query parameters as a Record<string, string>.
 * @returns A composed URL string with the domain, path, and query parameters.
 */
export const composePath = (
    domain: string,
    path: string,
    query: Record<string, string> | undefined
) => {
    if (!domain.endsWith('/')) domain += '/'
    if (path === 'index') path = ''

    if (!query || !Object.keys(query).length) return `${domain}${path}`

    let q = ''
    for (const [key, value] of Object.entries(query)) q += `${key}=${value}&`

    return `${domain}${path}?${q.slice(0, -1)}`
}

/**
 * Checks if a given message string represents a numeric value.
 * @param message The string to check.
 * @returns A boolean indicating if the string is numeric.
 */
export const isNumericString = (message: string) =>
	!Number.isNaN(parseInt(message))