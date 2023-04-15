extern crate amcl;
extern crate rand;

use super::amcl_utils::{
    self, ate2_evaluation, compress_g2, decompress_g2, g1mul, g2mul, hash_to_curve_g2, pair,
    subgroup_check_g2, AmclError, Big, GroupG1, GroupG2, G2_BYTES,
};
use super::keys::PublicKey;
use super::signature::Signature;
use rand::Rng;

/// Allows for the adding/combining of multiple BLS PublicKeys.
///
/// This may be used to verify some AggregateSignature.
///
/// AggregatePublicKeys MUST only be use after PoP Verifying the individual PublicKeys.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct AggregatePublicKey {
    pub point: GroupG1,
}

impl AggregatePublicKey {
    /// Instantiate a new aggregate public key from a vector of PublicKeys.
    ///
    /// This is a helper method combining the `new()` and `add()` functions.
    ///
    /// Pre-requsites: All public keys must be PoP verified before calling this function.
    pub fn aggregate(keys: &[&PublicKey]) -> Result<Self, AmclError> {
        if keys.is_empty() {
            return Err(AmclError::AggregateEmptyPoints);
        }

        let mut agg_key = Self {
            point: GroupG1::new(),
        };
        for key in keys {
            agg_key.point.add(&key.point)
        }
        Ok(agg_key)
    }

    /// Instantiate a new aggregate public key from a vector of PublicKeys.
    ///
    /// This is a helper method combining the `new()` and `add()` functions.
    ///
    /// Pre-requsites: All public keys must be PoP verified before calling this function.
    pub fn into_aggregate(keys: &[PublicKey]) -> Result<Self, AmclError> {
        if keys.is_empty() {
            return Err(AmclError::AggregateEmptyPoints);
        }

        let mut point = GroupG1::new();
        for key in keys {
            point.add(&key.point)
        }
        Ok(Self { point })
    }

    /// Instantiate a new aggregate public key from a single PublicKey.
    ///
    /// Pre-requsites: Public key must be PoP verified before calling this function.
    pub fn from_public_key(key: &PublicKey) -> Self {
        AggregatePublicKey {
            point: key.point.clone(),
        }
    }

    /// Add a PublicKey to the AggregatePublicKey.
    ///
    /// Pre-requsites: Public keys must be PoP verified before calling this function.
    pub fn add(&mut self, public_key: &PublicKey) {
        self.point.add(&public_key.point);
    }

    /// Add a AggregatePublicKey to the AggregatePublicKey.
    pub fn add_aggregate(&mut self, aggregate_public_key: &AggregatePublicKey) {
        // Note: it is possible to have an `AggregatePublicKey = infinity` by summing non-infinity PublicKeys
        // which add to a multiple of the group order.
        self.point.add(&aggregate_public_key.point);
    }
}

/// Allows for the adding/combining of multiple BLS Signatures.
///
/// This may be verified against some AggregatePublicKey.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct AggregateSignature {
    pub point: GroupG2,
}

impl AggregateSignature {
    /// Instantiates a new AggregateSignature.
    ///
    /// The underlying point will be set to infinity.
    pub fn new() -> Self {
        Self {
            point: GroupG2::new(),
        }
    }

    /// Instantiate a new AggregateSignature from a vector of Signatures.
    ///
    /// https://tools.ietf.org/html/draft-irtf-cfrg-bls-signature-02#section-2.8
    pub fn aggregate(signatures: &[&Signature]) -> Self {
        let mut aggregate_signature = AggregateSignature::new();
        for sig in signatures {
            aggregate_signature.point.add(&sig.point);
        }
        aggregate_signature
    }

    /// Instantiate a new AggregateSignature from a single Signature.
    pub fn from_signature(signature: &Signature) -> Self {
        AggregateSignature {
            point: signature.point.clone(),
        }
    }

    /// Add a Signature to the AggregateSignature.
    pub fn add(&mut self, signature: &Signature) {
        self.point.add(&signature.point);
    }

    /// Add a AggregateSignature to the AggregateSignature.
    ///
    /// To maintain consensus AggregateSignatures should only be added
    /// if they relate to the same message
    pub fn add_aggregate(&mut self, aggregate_signature: &AggregateSignature) {
        self.point.add(&aggregate_signature.point);
    }

    /// AggregateVerify
    ///
    /// Verifies an AggregateSignature against a list of Messages and PublicKeys
    /// https://tools.ietf.org/html/draft-irtf-cfrg-bls-signature-02#section-3.3
    pub fn aggregate_verify(&self, msgs: &[&[u8]], public_keys: &[&PublicKey]) -> bool {
        // Require same number of messages as PublicKeys and >=1 PublicKeys.
        if msgs.len() != public_keys.len() || public_keys.is_empty() {
            return false;
        }

        // Subgroup check for signature
        if !subgroup_check_g2(&self.point) {
            return false;
        }

        // Stores current value of pairings
        let mut pairing = pair::initmp();

        for (i, pk) in public_keys.iter().enumerate() {
            // Hash message to curve
            let mut msg_hash = hash_to_curve_g2(msgs[i]);

            // Points must be affine for pairing
            let mut pk_affine = pk.point.clone();
            pk_affine.affine();
            msg_hash.affine();

            // pairing *= e(H(msg[i], pk[i]))
            pair::another(&mut pairing, &msg_hash, &pk_affine);
        }

        // Affine for signature
        let mut sig_point = self.point.clone();
        let mut generator_g1_negative = amcl_utils::GroupG1::generator();
        sig_point.affine();
        generator_g1_negative.neg(); // already affine

        // pairing *= e(signature, G1)
        pair::another(&mut pairing, &sig_point, &generator_g1_negative);

        // Complete pairing and verify output is 1.
        let mut v = pair::miller(&pairing);
        v = pair::fexp(&v);
        v.is_unity()
    }

    /// FastAggregateVerify
    ///
    /// Verifies an AggregateSignature against a list of PublicKeys.
    /// PublicKeys must all be verified via Proof of Possession before running this function.
    /// https://tools.ietf.org/html/draft-irtf-cfrg-bls-signature-02#section-3.3.4
    pub fn fast_aggregate_verify(&self, msg: &[u8], public_keys: &[&PublicKey]) -> bool {
        // Require at least one PublicKey
        if public_keys.is_empty() {
            return false;
        }

        // Subgroup check for signature
        if !subgroup_check_g2(&self.point) {
            return false;
        }

        // Aggregate PublicKeys
        let aggregate_public_key = AggregatePublicKey::aggregate(public_keys);
        if aggregate_public_key.is_err() {
            return false;
        }
        let aggregate_public_key = aggregate_public_key.unwrap();

        // Ensure AggregatePublicKey is not infinity
        if aggregate_public_key.point.is_infinity() {
            return false;
        }

        // Hash message to curve
        let mut msg_hash = hash_to_curve_g2(msg);

        // Points must be affine for pairing
        let mut sig_point = self.point.clone();
        let mut key_point = aggregate_public_key.point;
        sig_point.affine();
        key_point.affine();
        msg_hash.affine();

        let mut generator_g1_negative = amcl_utils::GroupG1::generator();
        generator_g1_negative.neg(); // already affine

        // Faster ate2 evaualtion checks e(S, -G1) * e(H, PK) == 1
        ate2_evaluation(&sig_point, &generator_g1_negative, &msg_hash, &key_point)
    }

    /// FastAggregateVerify - pre-aggregated PublicKeys
    ///
    /// Verifies an AggregateSignature against an AggregatePublicKey.
    /// PublicKeys must all be verified before being aggregated.
    /// Differs to IEFT FastAggregateVerify in that public keys are already aggregated.
    /// https://tools.ietf.org/html/draft-irtf-cfrg-bls-signature-02#section-3.3.4
    pub fn fast_aggregate_verify_pre_aggregated(
        &self,
        msg: &[u8],
        aggregate_public_key: &AggregatePublicKey,
    ) -> bool {
        // Subgroup check for signature
        if !subgroup_check_g2(&self.point) {
            return false;
        }

        // Ensure AggregatePublicKey is not infinity
        if aggregate_public_key.point.is_infinity() {
            return false;
        }

        // Hash message to curve
        let mut msg_hash = hash_to_curve_g2(msg);

        // Points must be affine for pairing
        let mut sig_point = self.point.clone();
        let mut key_point = aggregate_public_key.point.clone();
        sig_point.affine();
        key_point.affine();
        msg_hash.affine();

        let mut generator_g1_negative = amcl_utils::GroupG1::generator();
        generator_g1_negative.neg(); // already affine

        // Faster ate2 evaualtion checks e(S, -G1) * e(H, PK) == 1
        ate2_evaluation(&sig_point, &generator_g1_negative, &msg_hash, &key_point)
    }

    /// Verify Multiple AggregateSignatures
    ///
    /// Input (AggregateSignature, PublicKey[m], Message(Vec<u8>))[n]
    /// Checks that each AggregateSignature is valid with a reduced number of pairings.
    /// https://ethresear.ch/t/fast-verification-of-multiple-bls-signatures/5407
    /// Note: Assumes Proof of Possession of public keys.
    pub fn verify_multiple_aggregate_signatures<'a, R, I>(rng: &mut R, signature_sets: I) -> bool
    where
        R: Rng + ?Sized,
        I: Iterator<Item = (&'a AggregateSignature, &'a AggregatePublicKey, &'a [u8])>,
    {
        // Sum of (AggregateSignature[i] * rand[i]) for all AggregateSignatures - S'
        let mut final_agg_sig = GroupG2::new();

        // Stores current value of pairings
        let mut pairing = pair::initmp();

        for (aggregate_signature, aggregate_public_key, message) in signature_sets {
            // Verify subgroup of each aggregate_signature
            if !subgroup_check_g2(&aggregate_signature.point) {
                return false;
            }

            // TODO: Consider increasing rand security from 2^63 to 2^128
            // Create random offset - rand[i]
            let mut rand = 0;
            while rand == 0 {
                // Require: rand > 0
                let mut rand_bytes = [0u8; 8]; // bytes
                rng.fill(&mut rand_bytes);
                rand = i64::from_be_bytes(rand_bytes).abs();
            }
            let rand = Big::new_int(rand as isize);

            // Hash message to curve - H(message[i])
            let mut msg_hash = hash_to_curve_g2(message);

            // rand[i] * Apk[i]
            let mut aggregate_public_key = g1mul(&aggregate_public_key.point, &rand);

            // Points must be affine before pairings
            msg_hash.affine();
            aggregate_public_key.affine();

            // Update current pairings: *= e(H(message[i]), rand[i] * Apk[i])
            pair::another(&mut pairing, &msg_hash, &aggregate_public_key);

            // S' += rand[i] * AggregateSignature[i]
            final_agg_sig.add(&g2mul(&aggregate_signature.point, &rand));
        }

        // Pairing for LHS - e(As', G1)
        let mut negative_g1 = GroupG1::generator();
        negative_g1.neg(); // will be affine
        final_agg_sig.affine();
        pair::another(&mut pairing, &final_agg_sig, &negative_g1);

        // Complete pairing and verify output is 1.
        let mut v = pair::miller(&pairing);
        v = pair::fexp(&v);
        v.is_unity()
    }

    /// Instatiate an AggregateSignature from some bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<AggregateSignature, AmclError> {
        let point = decompress_g2(bytes)?;
        Ok(Self { point })
    }

    /// Export (serialize) the AggregateSignature to bytes.
    pub fn as_bytes(&self) -> [u8; G2_BYTES] {
        compress_g2(&self.point)
    }
}

impl Default for AggregateSignature {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate hex;
    extern crate rand;

    use super::super::keys::{Keypair, SecretKey};
    use super::*;

    #[test]
    fn test_aggregate_serialization() {
        let signing_secret_key_bytes = vec![
            vec![
                98, 161, 50, 32, 254, 87, 16, 25, 167, 79, 192, 116, 176, 74, 164, 217, 40, 57,
                179, 15, 19, 21, 240, 100, 70, 127, 111, 170, 129, 137, 42, 53,
            ],
            vec![
                53, 72, 211, 104, 184, 68, 142, 208, 115, 22, 156, 97, 28, 216, 228, 102, 4, 218,
                116, 226, 166, 131, 67, 7, 40, 55, 157, 167, 157, 127, 143, 13,
            ],
        ];
        let signing_keypairs: Vec<Keypair> = signing_secret_key_bytes
            .iter()
            .map(|bytes| {
                let sk = SecretKey::from_bytes(&bytes).unwrap();
                let pk = PublicKey::from_secret_key(&sk);
                Keypair { sk, pk }
            })
            .collect();

        let message = "cats".as_bytes();

        let mut agg_sig = AggregateSignature::new();
        let mut public_keys = vec![];
        for keypair in &signing_keypairs {
            let sig = Signature::new(&message, &keypair.sk);
            agg_sig.add(&sig);
            public_keys.push(keypair.pk.clone());
        }

        let agg_pub_key = AggregatePublicKey::into_aggregate(&public_keys).unwrap();

        let agg_sig_bytes = agg_sig.as_bytes();
        let agg_sig = AggregateSignature::from_bytes(&agg_sig_bytes).unwrap();

        assert!(agg_sig.fast_aggregate_verify_pre_aggregated(&message, &agg_pub_key));
    }

    #[test]
    fn test_empty_fast_aggregate_verify() {
        let agg_sig = AggregateSignature::new();

        // Empty PublicKey array should fail
        assert!(!agg_sig.fast_aggregate_verify(&[0; 32], &[]));
    }

    #[test]
    fn test_split_zero_fast_aggregate_verify() {
        let agg_sig = AggregateSignature::new();

        let mut sk_bytes = [0; 32];
        sk_bytes[31] = 1;
        let sk = SecretKey::from_bytes(&sk_bytes).unwrap(); // 1
        let pk = PublicKey::from_secret_key(&sk);

        let sk_bytes =
            hex::decode("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000")
                .unwrap();
        let neg_sk = SecretKey::from_bytes(&sk_bytes).unwrap(); // -1
        let neg_pk = PublicKey::from_secret_key(&neg_sk);

        let public_keys = [&pk, &neg_pk];

        // Aggregates to zero should fail
        assert!(!agg_sig.fast_aggregate_verify(&[0; 32], &public_keys));
    }

    fn map_secret_bytes_to_keypairs(secret_key_bytes: Vec<Vec<u8>>) -> Vec<Keypair> {
        let mut keypairs = vec![];
        for bytes in secret_key_bytes {
            let sk = SecretKey::from_bytes(&bytes).unwrap();
            let pk = PublicKey::from_secret_key(&sk);
            keypairs.push(Keypair { sk, pk })
        }
        keypairs
    }

    // A helper for doing a comprehensive aggregate sig test.
    fn helper_test_aggregate_public_keys(
        control_kp: Keypair,
        signing_kps: Vec<Keypair>,
        non_signing_kps: Vec<Keypair>,
    ) {
        let signing_kps_subset = {
            let mut subset = vec![];
            for i in 0..signing_kps.len() - 1 {
                subset.push(signing_kps[i].clone());
            }
            subset
        };

        let messages = vec![
            "Small msg".as_bytes(),
            "cats lol".as_bytes(),
            &[42_u8; 133700],
        ];

        for message in messages {
            let mut agg_signature = AggregateSignature::new();
            let mut public_keys = vec![];

            for keypair in &signing_kps {
                let sig = Signature::new(&message, &keypair.sk);
                assert!(sig.verify(&message, &keypair.pk));
                assert!(!sig.verify(&message, &control_kp.pk));
                agg_signature.add(&sig);
                public_keys.push(keypair.pk.clone());
            }
            let signing_agg_pub = AggregatePublicKey::into_aggregate(&public_keys).unwrap();

            /*
             * The full set of signed keys should pass verification.
             */
            assert!(agg_signature.fast_aggregate_verify_pre_aggregated(&message, &signing_agg_pub));

            /*
             * The full set of signed keys aggregated in reverse order
             * should pass verification.
             */
            let rev_public_keys: Vec<&PublicKey> = public_keys.iter().rev().collect();
            let rev_signing_agg_pub = AggregatePublicKey::aggregate(&rev_public_keys).unwrap();
            assert!(
                agg_signature.fast_aggregate_verify_pre_aggregated(&message, &rev_signing_agg_pub)
            );

            /*
             * The signature should fail if an signing key has double-signed the
             * aggregate signature.
             */
            let mut double_sig_agg_sig = agg_signature.clone();
            let extra_sig = Signature::new(&message, &signing_kps[0].sk);
            double_sig_agg_sig.add(&extra_sig);
            assert!(!double_sig_agg_sig
                .fast_aggregate_verify_pre_aggregated(&message, &signing_agg_pub));

            /*
             * The full set of signed keys should fail verification if one key signs across a
             * different message.
             */
            let mut distinct_msg_agg_sig = AggregateSignature::new();
            let mut distinct_msg_public_keys = vec![];
            for (i, kp) in signing_kps.iter().enumerate() {
                let message = match i {
                    0 => "different_msg!1".as_bytes(),
                    _ => message,
                };
                let sig = Signature::new(&message, &kp.sk);
                distinct_msg_agg_sig.add(&sig);
                distinct_msg_public_keys.push(kp.pk.clone());
            }
            let distinct_msg_agg_pub = AggregatePublicKey::into_aggregate(&public_keys).unwrap();
            assert!(!distinct_msg_agg_sig
                .fast_aggregate_verify_pre_aggregated(&message, &distinct_msg_agg_pub));

            /*
             * The signature should fail if an extra, non-signing key has signed the
             * aggregate signature.
             */
            let mut super_set_agg_sig = agg_signature.clone();
            let extra_sig = Signature::new(&message, &non_signing_kps[0].sk);
            super_set_agg_sig.add(&extra_sig);
            assert!(
                !super_set_agg_sig.fast_aggregate_verify_pre_aggregated(&message, &signing_agg_pub)
            );

            /*
             * A subset of signed keys should fail verification.
             */
            let mut subset_pub_keys: Vec<&PublicKey> =
                signing_kps_subset.iter().map(|kp| &kp.pk).collect();
            let subset_agg_key =
                AggregatePublicKey::aggregate(&subset_pub_keys.as_slice()).unwrap();
            assert!(!agg_signature.fast_aggregate_verify_pre_aggregated(&message, &subset_agg_key));
            // Sanity check the subset test by completing the set and verifying it.
            subset_pub_keys.push(&signing_kps[signing_kps.len() - 1].pk);
            let subset_agg_key = AggregatePublicKey::aggregate(&subset_pub_keys).unwrap();
            assert!(agg_signature.fast_aggregate_verify_pre_aggregated(&message, &subset_agg_key));

            /*
             * A set of keys which did not sign the message at all should fail
             */
            let non_signing_pub_keys: Vec<&PublicKey> =
                non_signing_kps.iter().map(|kp| &kp.pk).collect();
            let non_signing_agg_key =
                AggregatePublicKey::aggregate(&non_signing_pub_keys.as_slice()).unwrap();
            assert!(
                !agg_signature.fast_aggregate_verify_pre_aggregated(&message, &non_signing_agg_key)
            );
        }
    }

    #[test]
    fn test_random_aggregate_public_keys() {
        let control_kp = Keypair::random(&mut rand::thread_rng());
        let signing_kps = vec![
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
        ];
        let non_signing_kps = vec![
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
            Keypair::random(&mut rand::thread_rng()),
        ];
        helper_test_aggregate_public_keys(control_kp, signing_kps, non_signing_kps);
    }

    #[test]
    fn test_known_aggregate_public_keys() {
        let control_secret_key_bytes = vec![vec![
            40, 129, 16, 229, 203, 159, 171, 37, 94, 38, 3, 24, 17, 213, 243, 246, 122, 105, 202,
            156, 186, 237, 54, 148, 116, 130, 20, 138, 15, 134, 45, 73,
        ]];
        let control_kps = map_secret_bytes_to_keypairs(control_secret_key_bytes);
        let control_kp = control_kps[0].clone();
        let signing_secret_key_bytes = vec![
            vec![
                98, 161, 50, 32, 254, 87, 16, 25, 167, 79, 192, 116, 176, 74, 164, 217, 40, 57,
                179, 15, 19, 21, 240, 100, 70, 127, 111, 170, 129, 137, 42, 53,
            ],
            vec![
                53, 72, 211, 104, 184, 68, 142, 208, 115, 22, 156, 97, 28, 216, 228, 102, 4, 218,
                116, 226, 166, 131, 67, 7, 40, 55, 157, 167, 157, 127, 143, 13,
            ],
            vec![
                94, 157, 163, 128, 239, 119, 116, 194, 162, 172, 189, 100, 36, 33, 13, 31, 137,
                177, 80, 73, 119, 126, 246, 215, 123, 178, 195, 12, 141, 65, 65, 89,
            ],
            vec![
                74, 195, 255, 195, 62, 36, 197, 48, 100, 25, 121, 8, 191, 219, 73, 136, 227, 203,
                98, 123, 204, 27, 197, 66, 193, 107, 115, 53, 5, 98, 137, 77,
            ],
            vec![
                82, 16, 65, 222, 228, 32, 47, 1, 245, 135, 169, 125, 46, 120, 57, 149, 121, 254,
                168, 52, 30, 221, 150, 186, 157, 141, 25, 143, 175, 196, 21, 176,
            ],
        ];
        let signing_kps = map_secret_bytes_to_keypairs(signing_secret_key_bytes);
        let non_signing_secret_key_bytes = vec![
            vec![
                6, 235, 126, 159, 58, 82, 170, 175, 73, 188, 251, 60, 79, 24, 164, 146, 88, 210,
                177, 65, 62, 183, 124, 129, 109, 248, 181, 29, 16, 128, 207, 23,
            ],
            vec![
                100, 177, 235, 229, 217, 215, 204, 237, 178, 196, 182, 51, 28, 147, 58, 24, 79,
                134, 41, 185, 153, 133, 229, 195, 32, 221, 247, 171, 91, 196, 65, 250,
            ],
            vec![
                65, 154, 236, 86, 178, 14, 179, 117, 113, 4, 40, 173, 150, 221, 23, 7, 117, 162,
                173, 104, 172, 241, 111, 31, 170, 241, 185, 31, 69, 164, 115, 126,
            ],
            vec![
                13, 67, 192, 157, 69, 188, 53, 161, 77, 187, 133, 49, 254, 165, 47, 189, 185, 150,
                23, 231, 143, 31, 64, 208, 134, 147, 53, 53, 228, 225, 104, 62,
            ],
            vec![
                22, 66, 26, 11, 101, 38, 37, 1, 148, 156, 162, 211, 37, 231, 37, 222, 172, 36, 224,
                218, 187, 127, 122, 195, 229, 234, 124, 91, 246, 73, 12, 120,
            ],
        ];
        let non_signing_kps = map_secret_bytes_to_keypairs(non_signing_secret_key_bytes);
        helper_test_aggregate_public_keys(control_kp, signing_kps, non_signing_kps);
    }

    #[test]
    pub fn add_aggregate_public_key() {
        let keypair_1 = Keypair::random(&mut rand::thread_rng());
        let keypair_2 = Keypair::random(&mut rand::thread_rng());
        let keypair_3 = Keypair::random(&mut rand::thread_rng());
        let keypair_4 = Keypair::random(&mut rand::thread_rng());

        let aggregate_public_key12 =
            AggregatePublicKey::aggregate(&[&keypair_1.pk, &keypair_2.pk]).unwrap();

        let aggregate_public_key34 =
            AggregatePublicKey::aggregate(&[&keypair_3.pk, &keypair_4.pk]).unwrap();

        // Should be the same as adding two aggregates
        let aggregate_public_key1234 = AggregatePublicKey::aggregate(&[
            &keypair_1.pk,
            &keypair_2.pk,
            &keypair_3.pk,
            &keypair_4.pk,
        ])
        .unwrap();

        // Aggregate AggregatePublicKeys
        let mut add_aggregate_public_key = aggregate_public_key12.clone();
        add_aggregate_public_key.add_aggregate(&aggregate_public_key34);

        assert_eq!(add_aggregate_public_key, aggregate_public_key1234);
    }

    #[test]
    pub fn add_aggregate_signature() {
        let msg: Vec<u8> = vec![1; 32];

        let keypair_1 = Keypair::random(&mut rand::thread_rng());
        let keypair_2 = Keypair::random(&mut rand::thread_rng());
        let keypair_3 = Keypair::random(&mut rand::thread_rng());
        let keypair_4 = Keypair::random(&mut rand::thread_rng());

        let sig_1 = Signature::new(&msg, &keypair_1.sk);
        let sig_2 = Signature::new(&msg, &keypair_2.sk);
        let sig_3 = Signature::new(&msg, &keypair_3.sk);
        let sig_4 = Signature::new(&msg, &keypair_4.sk);

        // Should be the same as adding two aggregates
        let aggregate_public_key = AggregatePublicKey::aggregate(&[
            &keypair_1.pk,
            &keypair_2.pk,
            &keypair_3.pk,
            &keypair_4.pk,
        ])
        .unwrap();

        let mut aggregate_signature = AggregateSignature::new();
        aggregate_signature.add(&sig_1);
        aggregate_signature.add(&sig_2);
        aggregate_signature.add(&sig_3);
        aggregate_signature.add(&sig_4);

        let mut add_aggregate_signature = AggregateSignature::new();
        add_aggregate_signature.add(&sig_1);
        add_aggregate_signature.add(&sig_2);

        let mut aggregate_signature34 = AggregateSignature::new();
        aggregate_signature34.add(&sig_3);
        aggregate_signature34.add(&sig_4);

        add_aggregate_signature.add_aggregate(&aggregate_signature34);

        add_aggregate_signature.point.affine();
        aggregate_signature.point.affine();

        assert_eq!(add_aggregate_signature, aggregate_signature);
        assert!(add_aggregate_signature
            .fast_aggregate_verify_pre_aggregated(&msg, &aggregate_public_key));
    }

    #[test]
    pub fn test_verify_multiple_signatures() {
        let mut rng = &mut rand::thread_rng();
        let n = 10; // Signatures
        let m = 3; // PublicKeys per Signature
        let mut msgs: Vec<Vec<u8>> = vec![vec![]; n];
        let mut aggregate_public_keys: Vec<AggregatePublicKey> = vec![];
        let mut aggregate_signatures: Vec<AggregateSignature> = vec![];

        let keypairs: Vec<Keypair> = (0..n * m).map(|_| Keypair::random(&mut rng)).collect();

        for i in 0..n {
            let mut public_keys = vec![];
            let mut aggregate_signature = AggregateSignature::new();
            msgs[i] = vec![i as u8; 32];
            for j in 0..m {
                let keypair = &keypairs[i * m + j];
                let signature = Signature::new(&msgs[i], &keypair.sk);

                public_keys.push(keypair.pk.clone());
                aggregate_signature.add(&signature);
            }
            let aggregate_public_key = AggregatePublicKey::into_aggregate(&public_keys).unwrap();
            aggregate_public_keys.push(aggregate_public_key);
            aggregate_signatures.push(aggregate_signature);
        }

        // Remove mutability
        let msgs: Vec<Vec<u8>> = msgs;
        let aggregate_public_keys: Vec<AggregatePublicKey> = aggregate_public_keys;
        let aggregate_signatures: Vec<AggregateSignature> = aggregate_signatures;

        // Create reference iterators
        let ref_vec = vec![1u8; 32];
        let ref_apk = aggregate_public_keys[0].clone();
        let ref_as = AggregateSignature::new();
        let mut msgs_refs: Vec<&[u8]> = vec![&ref_vec; n];
        let mut aggregate_public_keys_refs: Vec<&AggregatePublicKey> = vec![&ref_apk; n];
        let mut aggregate_signatures_refs: Vec<&AggregateSignature> = vec![&ref_as; n];

        for i in 0..n {
            msgs_refs[i] = &msgs[i];
            aggregate_signatures_refs[i] = &aggregate_signatures[i];
            aggregate_public_keys_refs[i] = &aggregate_public_keys[i];
        }

        let signature_sets = aggregate_signatures_refs
            .into_iter()
            .zip(aggregate_public_keys_refs)
            .zip(msgs_refs.iter().map(|x| *x))
            .map(|((a, b), c)| (a, b, c));

        let valid =
            AggregateSignature::verify_multiple_aggregate_signatures(&mut rng, signature_sets);

        assert!(valid);
    }

    #[test]
    pub fn test_verify_multiple_signatures_invalid() {
        let mut rng = &mut rand::thread_rng();
        let n = 10; // Signatures
        let m = 3; // PublicKeys per Signature
        let mut msgs: Vec<Vec<u8>> = vec![vec![]; n];
        let mut aggregate_public_keys: Vec<AggregatePublicKey> = vec![];
        let mut aggregate_signatures: Vec<AggregateSignature> = vec![];

        let keypairs: Vec<Keypair> = (0..n * m).map(|_| Keypair::random(&mut rng)).collect();

        // Deliberately use bad SecretKey
        let sk = SecretKey::from_bytes(&[1u8; 32]).unwrap();

        for i in 0..n {
            let mut public_keys = vec![];
            let mut aggregate_signature = AggregateSignature::new();
            msgs[i] = vec![i as u8; 32];
            for j in 0..m {
                let keypair = &keypairs[i * m + j];
                let signature = Signature::new(&msgs[i], &sk);

                public_keys.push(keypair.pk.clone());
                aggregate_signature.add(&signature);
            }
            let aggregate_public_key = AggregatePublicKey::into_aggregate(&public_keys).unwrap();
            aggregate_public_keys.push(aggregate_public_key);
            aggregate_signatures.push(aggregate_signature);
        }

        // Remove mutability
        let msgs: Vec<Vec<u8>> = msgs;
        let aggregate_public_keys: Vec<AggregatePublicKey> = aggregate_public_keys;
        let aggregate_signatures: Vec<AggregateSignature> = aggregate_signatures;

        // Create reference iterators
        let ref_vec = vec![1u8; 32];
        let ref_apk = aggregate_public_keys[0].clone();
        let ref_as = AggregateSignature::new();
        let mut msgs_refs: Vec<&[u8]> = vec![&ref_vec; n];
        let mut aggregate_public_keys_refs: Vec<&AggregatePublicKey> = vec![&ref_apk; n];
        let mut aggregate_signatures_refs: Vec<&AggregateSignature> = vec![&ref_as; n];

        for i in 0..n {
            msgs_refs[i] = &msgs[i];
            aggregate_signatures_refs[i] = &aggregate_signatures[i];
            aggregate_public_keys_refs[i] = &aggregate_public_keys[i];
        }

        let signature_sets = aggregate_signatures_refs
            .into_iter()
            .zip(aggregate_public_keys_refs)
            .zip(msgs_refs.iter().map(|x| *x))
            .map(|((a, b), c)| (a, b, c));

        let valid =
            AggregateSignature::verify_multiple_aggregate_signatures(&mut rng, signature_sets);

        // Should verify as false due to bad secret key
        assert!(!valid);
    }

    #[test]
    fn test_aggregate_verify() {
        let mut rng = &mut rand::thread_rng();
        let n = 10; // Number of signatures
        let mut msgs: Vec<Vec<u8>> = vec![vec![]; n];
        let mut public_keys: Vec<PublicKey> = vec![];
        let mut aggregate_signature = AggregateSignature::new();

        // Create keys and sign messages
        for i in 0..n {
            msgs[i] = vec![i as u8; 32];
            let key_pair = Keypair::random(&mut rng);
            let signature = Signature::new(&msgs[i], &key_pair.sk);

            public_keys.push(key_pair.pk);
            aggregate_signature.add(&signature);
        }

        // Convert to references
        let msgs_refs: Vec<&[u8]> = msgs.iter().map(|x| x.as_slice()).collect();
        let public_keys_refs: Vec<&PublicKey> = public_keys.iter().map(|x| x).collect();

        assert!(aggregate_signature.aggregate_verify(&msgs_refs, &public_keys_refs));
    }

    #[test]
    fn test_aggregate_verify_msg_repeat() {
        let mut rng = &mut rand::thread_rng();
        let n = 10; // Number of signatures
        let mut msgs: Vec<Vec<u8>> = vec![vec![]; n];
        let mut public_keys: Vec<PublicKey> = vec![];
        let mut aggregate_signature = AggregateSignature::new();

        // Create keys and sign messages
        for i in 0..n {
            // Deliberately repeat one message
            if i == n - 1 {
                msgs[i] = vec![0u8; 32];
            } else {
                msgs[i] = vec![i as u8; 32];
            }
            let key_pair = Keypair::random(&mut rng);
            let signature = Signature::new(&msgs[i], &key_pair.sk);

            public_keys.push(key_pair.pk);
            aggregate_signature.add(&signature);
        }

        // Convert to references
        let msgs_refs: Vec<&[u8]> = msgs.iter().map(|x| x.as_slice()).collect();
        let public_keys_refs: Vec<&PublicKey> = public_keys.iter().map(|x| x).collect();

        // Verification should be false due to repeated message
        assert!(aggregate_signature.aggregate_verify(&msgs_refs, &public_keys_refs));
    }

    #[test]
    fn test_aggregate_verify_invalid_signature() {
        let mut rng = &mut rand::thread_rng();
        let n = 10; // Number of signatures
        let mut msgs: Vec<Vec<u8>> = vec![vec![]; n];
        let mut public_keys: Vec<PublicKey> = vec![];
        let mut aggregate_signature = AggregateSignature::new();

        // Create keys and sign messages
        for i in 0..n {
            // Deliberately repeat one message
            msgs[i] = vec![i as u8; 32];
            let key_pair = Keypair::random(&mut rng);
            let signature = Signature::new(&msgs[i], &key_pair.sk);

            public_keys.push(key_pair.pk);

            // Deliberate don't add a signature
            if i != n - 1 {
                aggregate_signature.add(&signature);
            }
        }

        // Convert to references
        let msgs_refs: Vec<&[u8]> = msgs.iter().map(|x| x.as_slice()).collect();
        let public_keys_refs: Vec<&PublicKey> = public_keys.iter().map(|x| x).collect();

        // Verification should be false due to invalid signature
        assert!(!aggregate_signature.aggregate_verify(&msgs_refs, &public_keys_refs));
    }

    #[test]
    fn test_aggregate_verify_too_many_public_keys() {
        let mut rng = &mut rand::thread_rng();
        let msg = vec![1u8; 32];
        let mut public_keys: Vec<PublicKey> = vec![];
        let mut aggregate_signature = AggregateSignature::new();

        let key_pair = Keypair::random(&mut rng);
        let signature = Signature::new(&msg, &key_pair.sk);

        public_keys.push(key_pair.pk.clone());
        public_keys.push(key_pair.pk);

        aggregate_signature.add(&signature);

        // Convert to references
        let public_keys_refs: Vec<&PublicKey> = public_keys.iter().map(|x| x).collect();

        // Verification should be false due to too many public keys
        assert!(!aggregate_signature.aggregate_verify(&[&msg], &public_keys_refs));
    }

    #[test]
    fn test_aggregate_verify_too_many_messages() {
        let mut rng = &mut rand::thread_rng();
        let msg = vec![1u8; 32];
        let mut aggregate_signature = AggregateSignature::new();

        let key_pair = Keypair::random(&mut rng);
        let signature = Signature::new(&msg, &key_pair.sk);

        aggregate_signature.add(&signature);

        // Verification should be false due to too many messages
        assert!(!aggregate_signature.aggregate_verify(&[&msg, &msg], &[&key_pair.pk]));
    }

    #[test]
    fn test_from_public_key() {
        let multiplier = Big::new_int(5);
        let mut point = GroupG1::generator();
        point = point.mul(&multiplier);
        let public_key = PublicKey {
            point: point.clone(),
        };
        let aggregate_public_key = AggregatePublicKey::from_public_key(&public_key);

        assert_eq!(public_key.point, aggregate_public_key.point);
    }

    #[test]
    fn test_from_signature() {
        let multiplier = Big::new_int(5);
        let mut point = GroupG2::generator();
        point = point.mul(&multiplier);
        let signature = Signature { point };
        let aggregate_signature = AggregateSignature::from_signature(&signature);

        assert_eq!(signature.point, aggregate_signature.point);
    }

    #[test]
    fn test_readme() {
        // An exact replica of the README.md at the top level.
        let signing_secret_key_bytes = vec![
            vec![
                98, 161, 50, 32, 254, 87, 16, 25, 167, 79, 192, 116, 176, 74, 164, 217, 40, 57,
                179, 15, 19, 21, 240, 100, 70, 127, 111, 170, 129, 137, 42, 53,
            ],
            vec![
                53, 72, 211, 104, 184, 68, 142, 208, 115, 22, 156, 97, 28, 216, 228, 102, 4, 218,
                116, 226, 166, 131, 67, 7, 40, 55, 157, 167, 157, 127, 143, 13,
            ],
        ];

        // Load the key pairs from our serialized secret keys,
        let signing_keypairs: Vec<Keypair> = signing_secret_key_bytes
            .iter()
            .map(|bytes| {
                let sk = SecretKey::from_bytes(&bytes).unwrap();
                let pk = PublicKey::from_secret_key(&sk);
                Keypair { sk, pk }
            })
            .collect();

        let message = "cats".as_bytes();

        // Create an aggregate signature over some message, also generating an
        // aggregate public key at the same time.
        let mut agg_sig = AggregateSignature::new();
        let mut public_keys = vec![];
        for keypair in &signing_keypairs {
            let sig = Signature::new(&message, &keypair.sk);
            agg_sig.add(&sig);
            public_keys.push(keypair.pk.clone());
        }
        let agg_pub_key = AggregatePublicKey::into_aggregate(&public_keys).unwrap();

        // Serialize and de-serialize the aggregates, just 'cause we can.
        let agg_sig_bytes = agg_sig.as_bytes();
        let agg_sig = AggregateSignature::from_bytes(&agg_sig_bytes).unwrap();

        // Verify the AggregateSignature against the AggregatePublicKey
        assert!(agg_sig.fast_aggregate_verify_pre_aggregated(&message, &agg_pub_key));
    }
}
