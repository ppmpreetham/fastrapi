import React, { forwardRef } from "react"
import { Text, useTexture } from "@react-three/drei"
import { Mesh } from "three"

const PI = Math.PI
const front = 0.01
const back = -0.92

// front -> 0
// back -> 1
export const RotHeaderText = forwardRef<
  Mesh,
  {
    text: string
    place: boolean
    size?: number
    offset?: [number, number, number]
    color?: string
  }
>(({ text, place, size = 2, offset = [0, 0, 0], color = "#c9ff61" }, ref) => {
  return (
    <>
      {place ? (
        <Text
          ref={ref}
          color={color}
          fontSize={size}
          font="/fonts/random.ttf"
          position={[offset[0], offset[1], back + offset[2]]}
          rotation={[0, PI, 0]}
          outlineWidth={0.025}
          outlineColor="black"
        >
          {text}
        </Text>
      ) : (
        <Text
          ref={ref}
          color={color}
          fontSize={size}
          font="/fonts/random.ttf"
          position={[offset[0], offset[1], front + offset[2]]}
          outlineWidth={0.025}
          outlineColor="black"
        >
          {text}
        </Text>
      )}
    </>
  )
})

export const RotSubText = forwardRef<
  Mesh,
  {
    text: string
    place: boolean
    size?: number
    offset?: [number, number, number]
    color?: string
  }
>(({ text, place, size = 1, offset = [0, 0, 0], color = "#c9ff61" }, ref) => {
  return (
    <>
      {place ? (
        <Text
          ref={ref}
          color={color}
          fontSize={size}
          font="/fonts/random.ttf"
          position={[offset[0], offset[1], back + offset[2]]}
          rotation={[0, PI, 0]}
          outlineWidth={0.025}
          outlineColor="black"
        >
          {text}
        </Text>
      ) : (
        <Text
          ref={ref}
          color={color}
          fontSize={size}
          font="/fonts/random.ttf"
          position={[offset[0], offset[1], front + offset[2]]}
          outlineWidth={0.025}
          outlineColor="black"
        >
          {text}
        </Text>
      )}
    </>
  )
})

export const RotImage = forwardRef<Mesh, { src: string; place: boolean }>(({ src, place }, ref) => {
  const texture = useTexture(src)
  return (
    <>
      {place ? (
        <mesh ref={ref} position={[0, 0, back]} rotation={[0, PI, 0]}>
          <planeGeometry args={[4, 4]} />
          <meshBasicMaterial transparent map={texture} />
        </mesh>
      ) : (
        <mesh ref={ref} position={[0, 0, front]}>
          <planeGeometry args={[4, 4]} />
          <meshBasicMaterial transparent map={texture} />
        </mesh>
      )}
    </>
  )
})
