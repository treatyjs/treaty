/**
 * Represents an error response from the EdenClient, encapsulating the status code and error value.
 */
export class EdenFetchError<
    Status extends number = number,
    Value = unknown
> extends Error {
    status: Status
    value: Value

    constructor(status: Status, value: Value) {
        super()

        this.status = status
        this.value = value
    }
}