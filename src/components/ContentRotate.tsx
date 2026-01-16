import { RotHeaderText, RotSubText } from "./Texts"
import { Html } from "@react-three/drei"
import FrameworkChart from "./Chart"
import { useThree } from "@react-three/fiber"
import Features from "./Features"

var isMobile =
  typeof window !== "undefined" && /Mobi|Android|iPhone|iPad|iPod/i.test(navigator.userAgent)

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
        position={[3, 3.5, -1]}
        center
        scale={isMobile ? 0.7 : 0.3}
        transform
        rotation={[0, Math.PI, 0]}
        occlude={false}
        zIndexRange={[0, 0]}
        portal={{ current: gl.domElement.parentNode as HTMLElement }}
        className="fixed pointer-events-none"
      >
        <FrameworkChart />
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
      <RotHeaderText place={place} text="ONE CHARACTER" />
      {/* feat: show change from FastAPI to FastRAPI */}
    </group>
  )
}

export const Rotate4 = ({ place }: { place: boolean }) => {
  // Boxes rain down from the sky when clicked on the Check Docs

  // function PhysicsBoxes() {
  //   return <div>ContentRotate</div>;
  // }

  return (
    <group>
      <RotSubText
        place={place}
        text="5 mins to get started!"
        size={0.5}
        offset={[0, 1.4, 0]}
        color="white"
      />
      <mesh
        position={[0, 0, -0.01]}
        onClick={() => window.dispatchEvent(new Event("triggerRainDocs"))}
      >
        <planeGeometry args={[8, 2]} />
        <meshBasicMaterial color="#c6ff00" />
      </mesh>
      <RotHeaderText place={place} text="Check Docs" size={1.5} color="black" />
    </group>
  )
}
