import { useRef } from "react"
import gsap from "gsap"
import { useGSAP } from "@gsap/react"

const TRAIL_COUNT = 5
const BASE_SIZE = 40
const MAX_GROW = 40

const WIDTH_BLOCKS = 24
const HEIGHT_BLOCKS = 12

const Cursor = () => {
  const cursorRef = useRef<HTMLDivElement | null>(null)
  const trailRefs = useRef<HTMLDivElement[]>([])
  const hoveredRef = useRef(false)

  useGSAP(() => {
    const cursor = cursorRef.current
    if (!cursor) return

    let blockW = Math.max(1, window.innerWidth / WIDTH_BLOCKS)
    let blockH = Math.max(1, window.innerHeight / HEIGHT_BLOCKS)
    const onResize = () => {
      blockW = Math.max(1, window.innerWidth / WIDTH_BLOCKS)
      blockH = Math.max(1, window.innerHeight / HEIGHT_BLOCKS)
    }
    window.addEventListener("resize", onResize)

    type Pos = { x: number; y: number; speed: number; t: number }
    const positions: Pos[] = []

    let x = -1000,
      y = -1000
    let lastX = x,
      lastY = y
    let lastTime = performance.now()
    let raf = 0

    const isInteractive = (el: EventTarget | null) => {
      if (!(el instanceof Element)) return false
      const tag = el.tagName.toLowerCase()
      if (["a", "button", "input", "textarea", "select", "label"].includes(tag)) return true
      if (el.getAttribute && el.getAttribute("data-cursor") === "hover") return true
      return false
    }

    const onMove = (e: MouseEvent) => {
      const now = performance.now()
      const dt = Math.max(1, now - lastTime)
      lastTime = now

      lastX = x
      lastY = y
      x = e.clientX
      y = e.clientY
      const dx = x - lastX
      const dy = y - lastY

      const speed = (Math.sqrt(dx * dx + dy * dy) / dt) * 16

      const snapX = Math.round(x / blockW) * blockW
      const snapY = Math.round(y / blockH) * blockH

      positions.push({ x: snapX, y: snapY, speed, t: now })
      if (positions.length > TRAIL_COUNT) positions.shift()

      hoveredRef.current = isInteractive(e.target)

      for (let i = 0; i < TRAIL_COUNT; i++) {
        const el = trailRefs.current[i]
        if (!el) continue

        const idx = positions.length - 1 - i
        if (idx >= 0) {
          const pos = positions[idx]

          const s = Math.min(BASE_SIZE + MAX_GROW, Math.max(BASE_SIZE, BASE_SIZE + pos.speed * 2))

          let left = Math.round(pos.x - s / 2)
          let top = Math.round(pos.y - s / 2)
          left = Math.max(0, Math.min(left, Math.round(window.innerWidth - s)))
          top = Math.max(0, Math.min(top, Math.round(window.innerHeight - s)))

          const baseOpacity = Math.max(0.08, 0.7 - i * 0.13)
          const startOpacity = hoveredRef.current ? baseOpacity * 0.15 : baseOpacity

          gsap.set(el, { x: left, y: top, width: s, height: s, opacity: startOpacity })

          gsap.killTweensOf(el, "opacity")
          const fadeDuration = hoveredRef.current ? 0.6 : 3.0
          gsap.to(el, { opacity: 0, duration: fadeDuration, ease: "steps(20)" })
        } else {
          gsap.set(el, { x: -1000, y: -1000, width: BASE_SIZE, height: BASE_SIZE, opacity: 0 })
        }
      }
    }

    const onOver = (e: MouseEvent) => {
      hoveredRef.current = isInteractive(e.target)
    }
    const onOut = (e: MouseEvent) => {
      const related = (e as any).relatedTarget
      hoveredRef.current = isInteractive(related)
    }

    trailRefs.current.forEach((el) => gsap.set(el, { x: -1000, y: -1000, opacity: 0 }))
    gsap.set(cursor, { x: -1000, y: -1000 })

    const animate = () => {
      const last = positions.length ? positions[positions.length - 1] : null
      const v = last ? last.speed : 0
      const size = Math.min(BASE_SIZE + MAX_GROW, Math.max(BASE_SIZE, BASE_SIZE + v * 2))

      gsap.to(cursor, {
        x: x - size / 2,
        y: y - size / 2,
        width: size,
        height: size,
        duration: 0.12,
        ease: "steps(2)",
      })

      raf = requestAnimationFrame(animate)
    }

    document.addEventListener("mousemove", onMove)
    document.addEventListener("mouseover", onOver)
    document.addEventListener("mouseout", onOut)

    raf = requestAnimationFrame(animate)

    return () => {
      cancelAnimationFrame(raf)
      document.removeEventListener("mousemove", onMove)
      window.removeEventListener("resize", onResize)
      document.removeEventListener("mouseover", onOver)
      document.removeEventListener("mouseout", onOut)

      gsap.killTweensOf(cursor)
      trailRefs.current.forEach((el) => gsap.killTweensOf(el))
    }
  }, [])

  return (
    <>
      {/* trails */}
      {Array.from({ length: TRAIL_COUNT }).map((_, i) => (
        <div
          key={i}
          ref={(el) => {
            if (!el) return
            trailRefs.current[i] = el
          }}
          className={`fixed z-50 ${
            i % 2 === 1 ? "bg-primary" : "bg-secondary"
          } border border-black pointer-events-none`}
          style={{
            width: BASE_SIZE,
            height: BASE_SIZE,
            transform: "translate3d(-1000px,-1000px,0)",
            willChange: "transform, opacity, width, height",
            pointerEvents: "none",
          }}
        />
      ))}

      {/* cursor */}
      <div
        ref={cursorRef}
        className="fixed z-50 bg-primary border border-black pointer-events-none mix-blend-difference"
        style={{
          width: BASE_SIZE,
          height: BASE_SIZE,
          transform: "translate3d(-1000px,-1000px,0)",
          willChange: "transform, width, height",
          pointerEvents: "none",
        }}
      />
    </>
  )
}

export default Cursor
