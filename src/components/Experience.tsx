import { useRef, useState, useEffect } from "react"
import * as THREE from "three"
import { useFrame } from "@react-three/fiber"
import {
  PerspectiveCamera,
  OrbitControls,
  ScrollControls,
  useScroll,
  SpotLight,
} from "@react-three/drei"
import { Physics, RigidBody, CuboidCollider } from "@react-three/rapier"
import gsap from "gsap"
import { useGSAP } from "@gsap/react"

import { FastrapiModel } from "../models/fastrapimodel"
import { CrystalField, ProximityCrystals } from "../models/MultiCrystal"
import RainDown from "./RainDown"
import { Rotate0, Rotate1, Rotate2, Rotate3, Rotate4 } from "./ContentRotate"
import Buildings from "../models/Buildings"
import { CloudsMulti } from "../models/Cloud"

const rotateComponents = [Rotate0, Rotate1, Rotate2, Rotate3, Rotate4]
const length = rotateComponents.length
const PI = Math.PI
const radius = 10

const SNAP_ANGLES = Array.from({ length }, (_, k) => k * PI)

function getNearestSnapAngle(angle: number) {
  return SNAP_ANGLES.reduce((a, b) => (Math.abs(b - angle) < Math.abs(a - angle) ? b : a))
}

function CameraScroller({ cameraRef, tl, setCount }: any) {
  const scroll = useScroll()
  const angle = useRef(PI / 2)
  const prevOffset = useRef(scroll.offset)
  const lastMoveTime = useRef(performance.now())

  useFrame((_, delta) => {
    const rawAngle = scroll.offset * (length - 1) * PI

    const now = performance.now()
    const velocity = Math.abs(scroll.offset - prevOffset.current)
    prevOffset.current = scroll.offset

    if (velocity > 0.0001) lastMoveTime.current = now
    const isIdle = now - lastMoveTime.current > 1000

    const snap = getNearestSnapAngle(rawAngle)
    const target = isIdle ? snap : rawAngle

    angle.current = THREE.MathUtils.damp(angle.current, target, 6, delta)

    const index = SNAP_ANGLES.findIndex((a) => Math.abs(a - angle.current) < PI / 2)
    setCount(index === -1 ? 0 : index)

    const x = Math.sin(angle.current) * radius
    const z = Math.cos(angle.current) * radius
    cameraRef.current.position.set(x, 0, z)
    cameraRef.current.lookAt(0, 0, 0)

    tl.current.seek(scroll.offset * tl.current.duration())
  })

  return null
}

const Experience = () => {
  const tl = useRef<gsap.core.Timeline>(null!)
  const cameraRef = useRef<THREE.PerspectiveCamera>(null!)
  const [count, setCount] = useState(0)
  const [rainTriggered, setRainTriggered] = useState(false)

  useGSAP(() => {
    tl.current = gsap.timeline()
  }, [])

  useEffect(() => {
    const handler = () => {
      if (rainTriggered) return
      setRainTriggered(true)
      setTimeout(() => (window.location.href = "/docs"), 3500)
    }
    window.addEventListener("triggerRainDocs", handler)
    return () => window.removeEventListener("triggerRainDocs", handler)
  }, [rainTriggered])

  return (
    <>
      <OrbitControls enableZoom={false} enablePan={false} />
      <SpotLight position={[0, 5, 0]} angle={0.5} intensity={50} />

      <Physics gravity={[0, -20, 0]}>
        <ScrollControls pages={length} damping={0}>
          <PerspectiveCamera makeDefault ref={cameraRef} position={[0, 0, radius]} />
          <CameraScroller cameraRef={cameraRef} tl={tl} setCount={setCount} />

          <group>
            <RigidBody type="fixed" position={[0, -2, 0]} rotation={[-PI / 2, 0, 0]} friction={2}>
              <mesh>
                <planeGeometry args={[100, 100]} />
                <meshStandardMaterial wireframe color="#222" />
              </mesh>
              <CuboidCollider args={[50, 50, 1]} position={[0, 0, -1]} />
            </RigidBody>

            <CloudsMulti count={40} planeScale={100} offset={[5, 5, 0]} />
            <CrystalField count={50} planeScale={40} offset={-2} />
            <ProximityCrystals count={120} radius={35} />
            {rainTriggered && <RainDown planeScale={40} />}
            <Buildings
              min={[1, 1, 1]}
              max={[10, 10, 10]}
              avoidRadius={12}
              count={100}
              planeScale={75}
            />
            <FastrapiModel />
            {rotateComponents.map((C, i) =>
              count === i ? <C key={i} place={i % 2 === 1} /> : null
            )}
          </group>
        </ScrollControls>
      </Physics>
    </>
  )
}

export default Experience
