import { useEffect, useRef, useState } from "react"
import gsap from "gsap"
import { useGSAP } from "@gsap/react"
import { ScrollTrigger } from "gsap/all"
import { isMobile } from "../utils/helper"

gsap.registerPlugin(ScrollTrigger)

const DocsSection = () => {
  const [fontSize, setFontSize] = useState("9rem")
  const docsButtonRef = useRef<HTMLDivElement>(null)
  const docsRef = useRef<HTMLAnchorElement>(null)
  const checkRef = useRef<HTMLSpanElement>(null)

  const [rainTriggered, setRainTriggered] = useState(false)
  const [transitionComplete, setTransitionComplete] = useState(false)
  const [directDocsAccess, setDirectDocsAccess] = useState(false)

  useEffect(() => {
    const calculateFontSize = () => {
      const threejsTextSize = isMobile ? 0.75 : 1.5
      const cameraDistance = 10
      const cameraFov = 75

      const fovRadians = (cameraFov * Math.PI) / 180
      const visibleHeight = 2 * Math.tan(fovRadians / 2) * cameraDistance
      const pixelsPerUnit = window.innerHeight / visibleHeight

      const textHeightInUnits = threejsTextSize * 1.65
      const textHeightInPixels = textHeightInUnits * pixelsPerUnit

      setFontSize(`${textHeightInPixels}px`)
    }
    calculateFontSize()
    window.addEventListener("resize", calculateFontSize)
    return () => window.removeEventListener("resize", calculateFontSize)
  }, [])

  const animateToDocsState = (skipInitialDelay = false) => {
    const tl = gsap.timeline()

    if (skipInitialDelay) {
      gsap.set(docsButtonRef.current, {
        opacity: 1,
        width: "100vw",
        height: "100vh",
        backgroundColor: "#000000",
      })
      gsap.set(checkRef.current, {
        opacity: 1,
        color: "#c9ff61",
      })
      gsap.set(docsRef.current, {
        opacity: 1,
        color: "#c9ff61",
      })

      if (checkRef.current) {
        checkRef.current.innerText = "FastRAPI"
      }

      tl.to(docsRef.current, {
        x: `calc(-50vw + 4rem)`,
        y: `calc(-50vh + 2.5rem)`,
        scale: 0.25,
        duration: 1,
        onComplete: () => {
          setTransitionComplete(true)
        },
      })
    } else {
      tl.to(docsButtonRef.current, {
        width: "100vw",
        height: "100vh",
        duration: 1.5,
        ease: "power2.inOut",
        onComplete: () => {
          window.dispatchEvent(new Event("hideThreeJS"))
        },
      })
        .to(checkRef.current, {
          opacity: 0,
          duration: 0.5,
          onComplete: () => {
            if (checkRef.current) {
              checkRef.current.innerText = "FastRAPI"
            }
          },
        })
        .to(checkRef.current, {
          opacity: 1,
          color: "#c9ff61",
          duration: 1,
        })
        .to(
          docsRef.current,
          {
            opacity: 1,
            duration: 1,
            color: "#c9ff61",
          },
          "<",
        )
        .to(
          docsButtonRef.current,
          {
            backgroundColor: "#000000",
            duration: 1,
          },
          "<",
        )
        .to(docsRef.current, {
          x: `calc(-50vw + 4rem)`,
          y: `calc(-50vh + 2.5rem)`,
          scale: 0.25,
          duration: 1,
          onComplete: () => {
            setTransitionComplete(true)
            window.history.pushState({}, "", "/docs")
          },
        })
    }

    return tl
  }

  useEffect(() => {
    if (window.location.pathname === "/docs") {
      setDirectDocsAccess(true)
      setRainTriggered(true)

      window.dispatchEvent(new Event("hideThreeJS"))

      animateToDocsState(true)
    }
  }, [])

  useGSAP(() => {
    if (directDocsAccess) return

    gsap.set(docsButtonRef.current, { opacity: 0 })

    const handler = () => {
      if (rainTriggered) return
      setRainTriggered(true)

      const tl = gsap.timeline()

      tl.to(docsButtonRef.current, {
        opacity: 1,
        duration: 1,
        ease: "power2.out",
      }).add(() => {
        setTimeout(() => {
          animateToDocsState(false)
        }, 2500)
      })
    }

    window.addEventListener("triggerRainDocs", handler)
    return () => window.removeEventListener("triggerRainDocs", handler)
  }, [rainTriggered, directDocsAccess])

  return (
    <div className="fixed z-10 w-screen h-screen flex justify-center items-center font-random pointer-events-none will-change-transform">
      <div
        ref={docsButtonRef}
        className="bg-primary-glow text-black flex justify-center items-center"
        style={{ fontSize }}
      >
        <a ref={docsRef}>
          <span ref={checkRef}>Check</span> <span>Docs</span>
        </a>
      </div>
    </div>
  )
}

export default DocsSection
