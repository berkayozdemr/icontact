//! Numbers read off the face points: which way the head faces, how open the eyes are.

use crate::points;

/// Which way the head faces, in degrees. All zero is square-on to the camera.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HeadAngles {
    /// Turned sideways. Positive: towards the right of the picture.
    pub yaw: f32,
    /// Nodding. Positive: looking up.
    pub pitch: f32,
    /// Tilted towards a shoulder. Positive: clockwise in the picture.
    pub roll: f32,
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn unit(a: V3) -> Option<V3> {
    let len = dot(a, a).sqrt();
    (len > 1e-6).then(|| [a[0] / len, a[1] / len, a[2] / len])
}

/// Head angles from the mesh. `None` if the points are degenerate.
///
/// Uses the line across the face (cheek edge to cheek edge) and the line down
/// it (forehead to chin). The depth of the points is the model's guess, not a
/// measurement, so treat the result as good to a few degrees: enough to decide
/// "turned away too far", which is all the gate asks of it.
pub fn head_angles(p: &[[f32; 3]]) -> Option<HeadAngles> {
    let get = |i: usize| p.get(i).copied();
    let across = unit(sub(
        get(points::RIGHT_CHEEK_EDGE)?,
        get(points::LEFT_CHEEK_EDGE)?,
    ))?;
    let down = sub(get(points::CHIN)?, get(points::FOREHEAD)?);
    // keep only the part of "down" that is square to "across"
    let d = dot(down, across);
    let down = unit(sub(down, [across[0] * d, across[1] * d, across[2] * d]))?;
    // x is right, y is down, so across x down points away from the camera; the face looks the other way
    let back = cross(across, down);
    let facing = [-back[0], -back[1], -back[2]];
    Some(HeadAngles {
        yaw: facing[0].atan2(-facing[2]).to_degrees(),
        pitch: (-facing[1]).clamp(-1.0, 1.0).asin().to_degrees(),
        roll: across[1].atan2(across[0]).to_degrees(),
    })
}

fn dist(a: V3, b: V3) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// How open each eye is: lid gap divided by eye width, picture-left eye first.
/// About 0.25 to 0.35 when open, under 0.1 when shut.
pub fn eye_openness(p: &[[f32; 3]]) -> Option<[f32; 2]> {
    let one = |top: usize, bottom: usize, a: usize, b: usize| -> Option<f32> {
        let width = dist(*p.get(a)?, *p.get(b)?);
        let gap = dist(*p.get(top)?, *p.get(bottom)?);
        (width > 1e-6).then(|| gap / width)
    };
    Some([
        one(
            points::LEFT_EYE_TOP,
            points::LEFT_EYE_BOTTOM,
            points::LEFT_EYE_OUTER,
            points::LEFT_EYE_INNER,
        )?,
        one(
            points::RIGHT_EYE_TOP,
            points::RIGHT_EYE_BOTTOM,
            points::RIGHT_EYE_INNER,
            points::RIGHT_EYE_OUTER,
        )?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat face square-on to the camera, then turned by the given degrees.
    fn face(yaw: f32, pitch: f32, roll: f32) -> Vec<[f32; 3]> {
        let mut p = vec![[0.0; 3]; 478];
        p[points::LEFT_CHEEK_EDGE] = [-70.0, 0.0, 0.0];
        p[points::RIGHT_CHEEK_EDGE] = [70.0, 0.0, 0.0];
        p[points::FOREHEAD] = [0.0, -90.0, 0.0];
        p[points::CHIN] = [0.0, 90.0, 0.0];
        let (sy, cy) = yaw.to_radians().sin_cos();
        let (sp, cp) = pitch.to_radians().sin_cos();
        let (sr, cr) = roll.to_radians().sin_cos();
        for v in p.iter_mut() {
            let [x, y, z] = *v;
            // roll about the camera axis
            let (x, y) = (x * cr - y * sr, x * sr + y * cr);
            // pitch about the left-right axis: positive tips the forehead back
            let (y, z) = (y * cp + z * sp, -y * sp + z * cp);
            // yaw about the up-down axis: positive brings the picture-left cheek forward
            let (x, z) = (x * cy - z * sy, x * sy + z * cy);
            *v = [x, y, z];
        }
        p
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn square_on_is_zero() {
        assert_eq!(
            head_angles(&face(0.0, 0.0, 0.0)),
            Some(HeadAngles::default())
        );
    }

    #[test]
    fn each_angle_reads_back_alone() {
        let a = head_angles(&face(25.0, 0.0, 0.0)).unwrap();
        assert!(
            close(a.yaw, 25.0) && close(a.pitch, 0.0) && close(a.roll, 0.0),
            "{a:?}"
        );
        let a = head_angles(&face(0.0, -15.0, 0.0)).unwrap();
        assert!(
            close(a.yaw, 0.0) && close(a.pitch, -15.0) && close(a.roll, 0.0),
            "{a:?}"
        );
        let a = head_angles(&face(0.0, 0.0, 10.0)).unwrap();
        assert!(
            close(a.yaw, 0.0) && close(a.pitch, 0.0) && close(a.roll, 10.0),
            "{a:?}"
        );
    }

    #[test]
    fn turning_right_brings_the_left_cheek_nearer() {
        // a head turned towards picture-right shows its picture-left cheek to the camera
        let p = face(30.0, 0.0, 0.0);
        assert!(p[points::LEFT_CHEEK_EDGE][2] < p[points::RIGHT_CHEEK_EDGE][2]);
    }

    #[test]
    fn missing_points_give_none() {
        assert_eq!(head_angles(&[[0.0; 3]; 478]), None);
        assert_eq!(head_angles(&[]), None);
        assert_eq!(eye_openness(&[]), None);
    }

    #[test]
    fn eye_openness_is_gap_over_width() {
        let mut p = vec![[0.0; 3]; 478];
        p[points::LEFT_EYE_OUTER] = [100.0, 50.0, 0.0];
        p[points::LEFT_EYE_INNER] = [140.0, 50.0, 0.0];
        p[points::LEFT_EYE_TOP] = [120.0, 44.0, 0.0];
        p[points::LEFT_EYE_BOTTOM] = [120.0, 56.0, 0.0];
        p[points::RIGHT_EYE_INNER] = [180.0, 50.0, 0.0];
        p[points::RIGHT_EYE_OUTER] = [220.0, 50.0, 0.0];
        p[points::RIGHT_EYE_TOP] = [200.0, 49.0, 0.0];
        p[points::RIGHT_EYE_BOTTOM] = [200.0, 51.0, 0.0];
        let [l, r] = eye_openness(&p).unwrap();
        assert!(close(l, 0.3) && close(r, 0.05), "{l} {r}");
    }
}
