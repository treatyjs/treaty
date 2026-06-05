// React (.tsx) → Angular: A counter with useState, useEffect, props, and onClick handlers.
import { useState, useEffect } from 'react'

interface CounterProps {
  initialCount?: number
  onCountChange?: (count: number) => void
}

export default function Counter({ initialCount = 0, onCountChange }: CounterProps) {
  const [count, setCount] = useState(initialCount)
  const [isEven, setIsEven] = useState(initialCount % 2 === 0)

  // NAMED handler — referenced bare as `onClick={increment}` (never inline arrow).
  const increment = () => setCount((c) => c + 1)
  const decrement = () => setCount((c) => c - 1)
  const reset = () => setCount(initialCount)

  // useEffect to track even/odd and notify parent of changes.
  useEffect(() => {
    setIsEven(count % 2 === 0)
    if (onCountChange) {
      onCountChange(count)
    }
  }, [count, onCountChange])

  return (
    <div className="counter-card">
      <h2>Counter</h2>
      <div className="count-display">{count}</div>
      <p className="count-status">{isEven ? 'even' : 'odd'}</p>
      <div className="button-group">
        <button onClick={decrement}>−</button>
        <button onClick={reset}>Reset</button>
        <button onClick={increment}>+</button>
      </div>
    </div>
  )
}
