import { RotHeaderText, RotSubText } from "./Texts"
import { Html } from "@react-three/drei"
import FrameworkChart from "./Chart"
import { useThree } from "@react-three/fiber"
import Features from "./Features"
import { isMobile } from "../utils/helper"

export const Rotate0 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Welcome to" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText place={place} text="FastRAPI" size={isMobile ? 1 : 2} />
      <RotSubText place={place} text="Scroll to Continue" size={0.25} offset={[0, -4, 0]} />
    </group>
  )
}

export const Rotate1 = ({ place }: { place: boolean }) => {
  const { gl } = useThree()

  return (
    <group>
      <Html
        position={[isMobile ? 1.6 : 3, isMobile ? 2.5 : 3.5, -1]}
        center
        scale={isMobile ? 0.5 : 0.3}
        transform
        rotation={[0, Math.PI, 0]}
        occlude={false}
        zIndexRange={[0, 0]}
        portal={{ current: gl.domElement.parentNode as HTMLElement }}
        className="fixed pointer-events-none mix-blend-difference"
      >
        <FrameworkChart isMobile={isMobile} />
      </Html>
      <RotHeaderText
        place={place}
        text="33x Faster"
        offset={[0, 0, 0]}
        size={isMobile ? 0.85 : 2}
      />
      <RotHeaderText
        place={place}
        text="than FastAPI"
        size={0.5}
        offset={[0, -1, 0]}
        color="white"
      />
      <RotSubText
        place={place}
        text="* Represents Multiple Threads"
        size={0.25}
        offset={[0, -3.5, 0]}
        color="white"
      />
    </group>
  )
}

export const Rotate2 = ({ place }: { place: boolean }) => {
  return <Features place={place} />
}
export const Rotate3 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText place={place} text="Just change" size={0.5} offset={[0, 1, 0]} />
      <RotHeaderText
        place={place}
        text="ONE CHARACTER"
        size={isMobile ? 0.5 : 2}
        offset={isMobile ? [0, 0.35, 0] : [0, 0, 0]}
      />
      {/* feat: show change from FastAPI to FastRAPI */}
    </group>
  )
}

export const Rotate4 = ({ place }: { place: boolean }) => {
  return (
    <group>
      <RotSubText
        place={place}
        text="5 mins to get started!"
        size={isMobile ? 0.4 : 0.5}
        offset={[0, 1.4, 0]}
        color="white"
      />
      <mesh
        position={[0, isMobile ? 0.5 : 0, 0.01]}
        onClick={() => window.dispatchEvent(new Event("triggerRainDocs"))}
      >
        <planeGeometry args={isMobile ? [4, 1] : [8, 2]} />
        <meshBasicMaterial color="#c6ff00" />
      </mesh>
      <RotHeaderText
        place={place}
        text="Check Docs"
        size={isMobile ? 0.75 : 1.5}
        color="black"
        offset={[0, isMobile ? 0.5 : 0, 0.02]}
      />
    </group>
  )
}
