use crate::shared_math::fri::fri_blake3::Fri;
use crate::shared_math::mpolynomial::MPolynomial;
use crate::shared_math::other::log_2_ceil;
use crate::shared_math::prime_field_element_flexible::PrimeFieldElementFlexible;
use crate::shared_math::stark::stark_verify_error::{MerkleProofError, StarkVerifyError};
use crate::shared_math::traits::{CyclicGroupGenerator, GetPrimitiveRootOfUnity, IdentityValues};
use crate::shared_math::traits::{FromVecu8, ModPowU32};
use crate::util_types::blake3_wrapper::Blake3Hash;
use crate::util_types::merkle_tree::{MerkleTree, PartialAuthenticationPath};
use crate::util_types::simple_hasher::Hasher;
use crate::{shared_math::polynomial::Polynomial, util_types::proof_stream::ProofStream, utils};
use rand::{RngCore, SeedableRng};
use rand_pcg::Pcg64;
use std::fmt;
use std::iter::zip;
use std::{collections::HashMap, error::Error};

use super::stark_constraints_pfe_flexible::BoundaryConstraint;

pub const DOCUMENT_HASH_LENGTH: usize = 32usize;
pub const MERKLE_ROOT_HASH_LENGTH: usize = 32usize;

type StarkPfeHasher = blake3::Hasher;
type StarkPfeDigest = Blake3Hash;

#[derive(Clone, Debug)]
pub struct StarkPreprocessedValuesProver {
    transition_zerofier: Polynomial<PrimeFieldElementFlexible>,
    transition_zerofier_mt: MerkleTree<StarkPfeHasher>,
}

#[derive(Clone, Debug)]
pub struct StarkPreprocessedValues {
    transition_zerofier_mt_root: StarkPfeDigest,
    prover: Option<StarkPreprocessedValuesProver>,
}

#[derive(Clone, Debug)]
pub struct StarkPrimeFieldElementFlexible {
    expansion_factor: usize,
    fri: Fri<PrimeFieldElementFlexible, StarkPfeHasher>,
    field_generator: PrimeFieldElementFlexible,
    randomizer_count: usize,
    omega: PrimeFieldElementFlexible,
    pub omicron: PrimeFieldElementFlexible, // omicron = omega^expansion_factor
    omicron_domain: Vec<PrimeFieldElementFlexible>,
    omicron_domain_length: usize,
    original_trace_length: usize,
    randomized_trace_length: usize,
    register_count: usize,
    preprocessed_values: Option<StarkPreprocessedValues>,
}

impl StarkPrimeFieldElementFlexible {
    pub fn new(
        expansion_factor: usize,
        colinearity_check_count: usize,
        register_count: usize,
        cycle_count: usize,
        transition_constraints_degree: usize,
        generator: PrimeFieldElementFlexible,
    ) -> Self {
        let num_randomizers = 4 * colinearity_check_count;
        let original_trace_length = cycle_count;
        let randomized_trace_length = original_trace_length + num_randomizers;
        let omicron_domain_length =
            1usize << log_2_ceil((randomized_trace_length * transition_constraints_degree) as u128);
        let fri_domain_length = omicron_domain_length * expansion_factor;
        let omega = generator
            .get_primitive_root_of_unity(fri_domain_length as u64)
            .0
            .unwrap();
        let omicron = omega.mod_pow(expansion_factor.into());

        // Verify omega and omicron values
        assert!(
            omicron.mod_pow(omicron_domain_length.into()).is_one(),
            "omicron must have correct order"
        );
        assert!(
            !omicron.mod_pow((omicron_domain_length / 2).into()).is_one(),
            "omicron must have correct primitive order"
        );
        assert!(
            omega.mod_pow(fri_domain_length.into()).is_one(),
            "omicron must have correct order"
        );
        assert!(
            !omega.mod_pow((fri_domain_length / 2).into()).is_one(),
            "omicron must have correct primitive order"
        );

        let omicron_domain = omicron.get_cyclic_group_elements(None);

        let fri = Fri::<PrimeFieldElementFlexible, StarkPfeHasher>::new(
            generator,
            omega,
            fri_domain_length,
            expansion_factor,
            colinearity_check_count,
        );

        Self {
            expansion_factor,
            field_generator: generator,
            randomizer_count: num_randomizers,
            omega,
            omicron,
            omicron_domain,
            omicron_domain_length,
            original_trace_length,
            randomized_trace_length,
            register_count,
            fri,
            preprocessed_values: None,
        }
    }

    /// Set the transition zerofier merkle tree root needed by the verifier
    /// This is a trusted function where the input value cannot be provided by the prover
    /// or by an untrusted 3rd party.
    pub fn set_transition_zerofier_mt_root(&mut self, transition_zerofier_mt_root: StarkPfeDigest) {
        self.preprocessed_values = Some(StarkPreprocessedValues {
            transition_zerofier_mt_root,
            prover: None,
        });
    }

    // Compute and set the preprocess values for both the prover and verifier, not a trusted
    // function as all functions are computed locally
    pub fn prover_preprocess(&mut self) {
        let hasher = StarkPfeHasher::new();
        let transition_zerofier: Polynomial<PrimeFieldElementFlexible> = Polynomial::fast_zerofier(
            &self.omicron_domain[..self.original_trace_length - 1],
            &self.omicron,
            self.omicron_domain.len(),
        );

        let transition_zerofier_codeword: Vec<PrimeFieldElementFlexible> = transition_zerofier
            .fast_coset_evaluate(&self.field_generator, self.omega, self.fri.domain.length);

        let transition_zerofier_codeword_digests: Vec<_> = transition_zerofier_codeword
            .iter()
            .map(|x| hasher.hash(x))
            .collect();
        let transition_zerofier_mt =
            MerkleTree::<StarkPfeHasher>::from_digests(&transition_zerofier_codeword_digests);
        let transition_zerofier_mt_root = transition_zerofier_mt.get_root();

        self.preprocessed_values = Some(StarkPreprocessedValues {
            transition_zerofier_mt_root,
            prover: Some(StarkPreprocessedValuesProver {
                transition_zerofier,
                transition_zerofier_mt,
            }),
        });
    }

    pub fn ready_for_verify(&self) -> bool {
        self.preprocessed_values.is_some()
    }

    pub fn ready_for_prove(&self) -> bool {
        match &self.preprocessed_values {
            None => false,
            Some(preprocessed_values) => preprocessed_values.prover.is_some(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StarkProofError {
    InputOutputMismatch,
    HighDegreeExtendedComputationalTrace,
    HighDegreeBoundaryQuotient,
    HighDegreeTransitionQuotient,
    HighDegreeLinearCombination,
    MissingPreprocessedValues,
    NonZeroBoundaryRemainder,
    NonZeroTransitionRemainder,
}

impl Error for StarkProofError {}

impl fmt::Display for StarkProofError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

// Return the interpolants for the provided points. This is the `L(x)` in the equation
// to derive the boundary quotient: `Q_B(x) = (ECT(x) - L(x)) / Z_B(x)`.
// input is indexed with bcs[register][cycle]
fn get_boundary_interpolants(
    bcs: Vec<Vec<(PrimeFieldElementFlexible, PrimeFieldElementFlexible)>>,
) -> Vec<Polynomial<PrimeFieldElementFlexible>> {
    bcs.iter()
        .map(|points| Polynomial::slow_lagrange_interpolation(points))
        .collect()
}

fn get_boundary_zerofiers(
    bcs: Vec<Vec<(PrimeFieldElementFlexible, PrimeFieldElementFlexible)>>,
    one: PrimeFieldElementFlexible,
) -> Vec<Polynomial<PrimeFieldElementFlexible>> {
    let roots: Vec<Vec<PrimeFieldElementFlexible>> = bcs
        .iter()
        .map(|points| points.iter().map(|(x, _y)| x.to_owned()).collect())
        .collect();
    roots
        .iter()
        .map(|points| Polynomial::get_polynomial_with_roots(points, one))
        .collect()
}

impl StarkPrimeFieldElementFlexible {
    // Return the degrees of the boundary quotients
    fn boundary_quotient_degree_bounds(
        &self,
        boundary_zerofiers: &[Polynomial<PrimeFieldElementFlexible>],
    ) -> Vec<usize> {
        let transition_degree = self.randomized_trace_length - 1;
        boundary_zerofiers
            .iter()
            .map(|x| transition_degree - x.degree() as usize)
            .collect()
    }

    // Return the max degree for all interpolations of the execution trace
    fn transition_degree_bounds(
        &self,
        transition_constraints: &[MPolynomial<PrimeFieldElementFlexible>],
    ) -> Vec<usize> {
        let mut point_degrees = vec![0; 1 + 2 * self.register_count];
        point_degrees[0] = 1;
        for r in 0..self.register_count {
            point_degrees[1 + r] = self.randomized_trace_length - 1;
            point_degrees[1 + r + self.register_count] = self.randomized_trace_length - 1;
        }

        // This could also be achieved with symbolic evaluation of the
        // input `transition_constraints` and then taking the degree of
        // the resulting polynomials.
        let mut res: Vec<usize> = vec![];
        for a in transition_constraints {
            let mut max_degree = 0usize;
            for (k, _) in a.coefficients.iter() {
                let mut acc = 0;
                for (r, l) in point_degrees.iter().zip(k.iter()) {
                    acc += *r * (*l as usize);
                }
                if acc > max_degree {
                    max_degree = acc;
                }
            }
            res.push(max_degree);
        }

        res
    }

    /// Return the max degree for all transition quotients
    /// This is the degree of the execution trace interpolations
    /// divided by the transition zerofier polynomial
    fn transition_quotient_degree_bounds(
        &self,
        transition_constraints: &[MPolynomial<PrimeFieldElementFlexible>],
    ) -> Vec<usize> {
        // The degree is the degree of the trace plus the randomizers
        // minus the original trace length minus 1.
        self.transition_degree_bounds(transition_constraints)
            .iter()
            .map(|d| d - (self.original_trace_length - 1))
            .collect()
    }

    /// Return the degree of the combination polynomial, this is the degree limit,
    /// that is proven by FRI
    fn max_degree(
        &self,
        transition_constraints: &[MPolynomial<PrimeFieldElementFlexible>],
    ) -> usize {
        let tqdbs: Vec<usize> = self.transition_quotient_degree_bounds(transition_constraints);
        let md_res = tqdbs.iter().max();
        let md = md_res.unwrap();
        // Round up to nearest 2^k - 1
        let l2 = log_2_ceil(*md as u128);
        (1 << l2) - 1
    }

    fn sample_weights(&self, randomness: &[u8], number: usize) -> Vec<PrimeFieldElementFlexible> {
        let k_seeds = utils::get_n_hash_rounds(randomness, number as u32);
        k_seeds
            .iter()
            .map(|seed| self.omega.from_vecu8(seed.to_vec()))
            .collect()
    }

    // Convert boundary constraints into a vector of boundary
    // constraints indexed by register.
    fn format_boundary_constraints(
        &self,
        boundary_constraints: Vec<BoundaryConstraint>,
    ) -> Vec<Vec<(PrimeFieldElementFlexible, PrimeFieldElementFlexible)>> {
        let mut bcs: Vec<Vec<(PrimeFieldElementFlexible, PrimeFieldElementFlexible)>> =
            vec![vec![]; self.register_count];
        for bc in boundary_constraints {
            bcs[bc.register].push((self.omicron.mod_pow(bc.cycle.into()), bc.value));
        }

        bcs
    }

    pub fn prove(
        &self,
        // Trace is indexed as trace[cycle][register]
        trace: Vec<Vec<PrimeFieldElementFlexible>>,
        transition_constraints: Vec<MPolynomial<PrimeFieldElementFlexible>>,
        boundary_constraints: Vec<BoundaryConstraint>,
        proof_stream: &mut ProofStream,
    ) -> Result<(), Box<dyn Error>> {
        let hasher = StarkPfeHasher::new();

        if !self.ready_for_prove() {
            return Err(Box::new(StarkProofError::MissingPreprocessedValues));
        }

        let transition_zerofier_polynomial: Polynomial<PrimeFieldElementFlexible> = self
            .preprocessed_values
            .as_ref()
            .unwrap()
            .prover
            .as_ref()
            .unwrap()
            .transition_zerofier
            .clone();
        let transition_zerofier_mt: MerkleTree<StarkPfeHasher> = self
            .preprocessed_values
            .as_ref()
            .unwrap()
            .prover
            .as_ref()
            .unwrap()
            .transition_zerofier_mt
            .clone();

        // Concatenate randomizers
        // TODO: PCG ("permuted congrential generator") is not cryptographically secure; so exchange this for something else like Keccak/SHAKE256
        let mut rng = Pcg64::seed_from_u64(17);
        let mut rand_bytes = [0u8; 32];
        let mut randomized_trace: Vec<Vec<PrimeFieldElementFlexible>> = trace;
        for _ in 0..self.randomizer_count {
            randomized_trace.push(vec![]);
            for _ in 0..self.register_count {
                rng.fill_bytes(&mut rand_bytes);
                randomized_trace
                    .last_mut()
                    .unwrap()
                    .push(self.omega.from_vecu8(rand_bytes.to_vec()));
            }
        }

        // Interpolate the trace to get a polynomial going through all
        // trace values
        let randomized_trace_domain: Vec<PrimeFieldElementFlexible> = self
            .omicron
            .get_cyclic_group_elements(Some(randomized_trace.len()));
        let mut trace_polynomials = vec![];
        for r in 0..self.register_count {
            trace_polynomials.push(Polynomial::fast_interpolate(
                &randomized_trace_domain,
                &randomized_trace
                    .iter()
                    .map(|t| t[r])
                    .collect::<Vec<PrimeFieldElementFlexible>>(),
                &self.omicron,
                self.omicron_domain_length,
            ));
        }

        // Subtract boundary interpolants and divide out boundary zerofiers
        let bcs_formatted = self.format_boundary_constraints(boundary_constraints);
        let boundary_interpolants: Vec<Polynomial<PrimeFieldElementFlexible>> =
            get_boundary_interpolants(bcs_formatted.clone());
        let boundary_zerofiers: Vec<Polynomial<PrimeFieldElementFlexible>> =
            get_boundary_zerofiers(bcs_formatted, self.omega.ring_one());
        let mut boundary_quotients: Vec<Polynomial<PrimeFieldElementFlexible>> =
            vec![Polynomial::ring_zero(); self.register_count];
        for r in 0..self.register_count {
            let div_res = (trace_polynomials[r].clone() - boundary_interpolants[r].clone())
                .divide(boundary_zerofiers[r].clone());
            assert!(
                div_res.1.is_zero(),
                "Remainder must be zero when dividing out boundary zerofier"
            );
            boundary_quotients[r] = div_res.0;
        }

        // Commit to boundary quotients
        let mut boundary_quotient_merkle_trees: Vec<MerkleTree<StarkPfeHasher>> = vec![];
        let mut boundary_quotient_codewords: Vec<Vec<PrimeFieldElementFlexible>> = vec![];
        for bq in boundary_quotients.iter() {
            let boundary_quotient_codeword: Vec<PrimeFieldElementFlexible> =
                bq.fast_coset_evaluate(&self.field_generator, self.omega, self.fri.domain.length);
            boundary_quotient_codewords.push(boundary_quotient_codeword.clone());
            let boundary_quotient_codeword_digests: Vec<_> = boundary_quotient_codeword
                .iter()
                .map(|x| hasher.hash(x))
                .collect();
            let bq_merkle_tree = MerkleTree::from_digests(&boundary_quotient_codeword_digests);
            proof_stream.enqueue(&bq_merkle_tree.get_root())?;
            boundary_quotient_merkle_trees.push(bq_merkle_tree);
        }

        // Symbolically evaluate transition constraints
        let x = Polynomial {
            coefficients: vec![self.omega.ring_zero(), self.omega.ring_one()],
        };
        let mut point: Vec<Polynomial<PrimeFieldElementFlexible>> = vec![x];

        // add polynomial representing trace[x_i] and trace[x_{i+1}]
        point.append(&mut trace_polynomials.clone());
        point.append(
            &mut trace_polynomials
                .clone() // TODO: REMOVE
                .into_iter()
                .map(|tp| tp.scale(&self.omicron))
                .collect(),
        );
        let transition_polynomials: Vec<Polynomial<PrimeFieldElementFlexible>> =
            transition_constraints
                .iter()
                .map(|tc| tc.evaluate_symbolic(&point))
                .collect();

        // divide out transition zerofier
        let transition_quotients: Vec<Polynomial<PrimeFieldElementFlexible>> =
            transition_polynomials
                .iter()
                .map(|tp| {
                    Polynomial::fast_coset_divide(
                        tp,
                        &transition_zerofier_polynomial,
                        self.field_generator,
                        self.omicron,
                        self.omicron_domain.len(),
                    )
                })
                .collect();

        // Commit to randomizer polynomial
        let max_degree = self.max_degree(&transition_constraints);
        let mut randomizer_polynomial_coefficients: Vec<PrimeFieldElementFlexible> = vec![];
        for _ in 0..max_degree + 1 {
            let mut coefficient_rand_bytes = [0u8; 32];
            rng.fill_bytes(&mut coefficient_rand_bytes);
            randomizer_polynomial_coefficients
                .push(self.omega.from_vecu8(coefficient_rand_bytes.to_vec()));
        }

        let randomizer_polynomial = Polynomial {
            coefficients: randomizer_polynomial_coefficients,
        };

        let randomizer_codeword: Vec<PrimeFieldElementFlexible> = randomizer_polynomial
            .fast_coset_evaluate(&self.field_generator, self.omega, self.fri.domain.length);
        let randomizer_codeword_digests: Vec<_> = randomizer_codeword
            .iter()
            .map(|rand_eval| hasher.hash(rand_eval))
            .collect();
        let randomizer_mt =
            MerkleTree::<StarkPfeHasher>::from_digests(&randomizer_codeword_digests);
        proof_stream.enqueue(&randomizer_mt.get_root())?;

        // Sanity check, should probably be removed
        let expected_tq_degrees = self.transition_quotient_degree_bounds(&transition_constraints);
        for r in 0..self.register_count {
            assert_eq!(
                expected_tq_degrees[r] as isize,
                transition_quotients[r].degree(),
                "Transition quotient degree must match expected value"
            );
        }

        // Compute terms of nonlinear combination polynomial
        let boundary_degrees = self.boundary_quotient_degree_bounds(&boundary_zerofiers);
        let mut terms: Vec<Polynomial<PrimeFieldElementFlexible>> = vec![randomizer_polynomial];
        for (tq, tq_degree) in transition_quotients.iter().zip(expected_tq_degrees.iter()) {
            terms.push(tq.to_owned());
            let shift = max_degree - tq_degree;
            let shifted = tq.shift_coefficients(shift, self.omega.ring_zero());
            assert_eq!(max_degree as isize, shifted.degree()); // TODO: Can be removed
            terms.push(shifted);
        }
        for (bq, bq_degree) in boundary_quotients.iter().zip(boundary_degrees.iter()) {
            terms.push(bq.to_owned());
            let shift = max_degree - bq_degree;
            let shifted = bq.shift_coefficients(shift, self.omega.ring_zero());
            assert_eq!(max_degree as isize, shifted.degree()); // TODO: Can be removed
            terms.push(shifted);
        }

        // Take weighted sum
        // # get weights for nonlinear combination
        // #  - 1 randomizer
        // #  - 2 for every transition quotient
        // #  - 2 for every boundary quotient
        let fiat_shamir_hash: Vec<u8> = proof_stream.prover_fiat_shamir();
        let weights = self.sample_weights(
            &fiat_shamir_hash,
            1 + 2 * transition_quotients.len() + 2 * boundary_quotients.len(),
        );
        assert_eq!(
            weights.len(),
            terms.len(),
            "weights and terms length must match"
        );
        let combination = weights
            .iter()
            .zip(terms.iter())
            .fold(Polynomial::ring_zero(), |sum, (weight, pol)| {
                sum + pol.scalar_mul(weight.to_owned())
            });

        let combined_codeword = combination.fast_coset_evaluate(
            &self.field_generator,
            self.omega,
            self.fri.domain.length,
        );

        // Prove low degree of combination polynomial, and collect indices
        let indices: Vec<usize> = self.fri.prove(&combined_codeword, proof_stream)?;

        // Process indices
        let mut duplicated_indices = indices.clone();
        duplicated_indices.append(
            &mut indices
                .into_iter()
                .map(|i| (i + self.expansion_factor) % self.fri.domain.length)
                .collect(),
        );
        let mut quadrupled_indices = duplicated_indices.clone();
        quadrupled_indices.append(
            &mut duplicated_indices
                .clone()
                .into_iter()
                .map(|i| (i + self.fri.domain.length / 2) % self.fri.domain.length)
                .collect(),
        );
        quadrupled_indices.sort_unstable();
        println!("quad {:?  }.", quadrupled_indices);

        // Open indicated positions in the boundary quotient codewords
        for (j, bq_mt) in boundary_quotient_merkle_trees.into_iter().enumerate() {
            let auth_paths = bq_mt.get_multi_proof(&quadrupled_indices);
            //let leaves = bq_mt.get_leaves_by_indices(&quadrupled_indices);
            let values: Vec<PrimeFieldElementFlexible> = quadrupled_indices
                .iter()
                .map(|i| boundary_quotient_codewords[j][*i])
                .collect();
            assert_eq!(auth_paths.len(), values.len());
            /*
            let ap_value_pairs: Vec<(
                PartialAuthenticationPath<StarkPfeDigest>,
                PrimeFieldElementFlexible,
            )> = zip(auth_paths, values).collect();
            */
            proof_stream.enqueue_length_prepended(&auth_paths)?;
            proof_stream.enqueue_length_prepended(&values)?;
        }

        // Open indicated positions in the randomizer
        let randomizer_proof: Vec<PartialAuthenticationPath<StarkPfeDigest>> =
            randomizer_mt.get_multi_proof(&quadrupled_indices);
        let randomizer_proof_values: Vec<PrimeFieldElementFlexible> = quadrupled_indices
            .iter()
            .map(|&i| randomizer_codeword[i])
            .collect();
        proof_stream.enqueue_length_prepended(&randomizer_proof)?;
        proof_stream.enqueue_length_prepended(&randomizer_proof_values)?;

        // Open indicated positions in the zerofier
        let transition_zerofier_proof: Vec<PartialAuthenticationPath<StarkPfeDigest>> =
            transition_zerofier_mt.get_multi_proof(&quadrupled_indices);
        proof_stream.enqueue_length_prepended(&transition_zerofier_proof)?;

        let transition_zerofier_proof_values: Vec<PrimeFieldElementFlexible> = quadrupled_indices
            .iter()
            .map(|i| {
                transition_zerofier_polynomial
                    .evaluate(&(self.field_generator * self.omega.mod_pow_u32(*i as u32)))
            })
            .collect();

        // let transition_zerofier_proof_values: Vec<StarkPfeDigest> = todo!();
        proof_stream.enqueue_length_prepended(&transition_zerofier_proof_values)?;

        Ok(())
    }

    pub fn verify(
        &self,
        proof_stream: &mut ProofStream,
        transition_constraints: Vec<MPolynomial<PrimeFieldElementFlexible>>,
        boundary_constraints: Vec<BoundaryConstraint>,
    ) -> Result<(), Box<dyn Error>> {
        if !self.ready_for_verify() {
            return Err(Box::new(StarkVerifyError::MissingPreprocessedValues));
        }
        let hasher = StarkPfeHasher::new();

        let transition_zerofier_mt_root: StarkPfeDigest = self
            .preprocessed_values
            .as_ref()
            .unwrap()
            .transition_zerofier_mt_root;

        // Get Merkle root of boundary quotient codewords
        let mut boundary_quotient_mt_roots: Vec<StarkPfeDigest> = vec![];
        for _ in 0..self.register_count {
            boundary_quotient_mt_roots.push(proof_stream.dequeue(32)?);
        }

        let randomizer_mt_root: StarkPfeDigest = proof_stream.dequeue(32)?;

        // Get weights for nonlinear combination
        // 1 weight element for randomizer
        // 2 for every transition quotient
        // 2 for every boundary quotient
        let fiat_shamir_hash: Vec<u8> = proof_stream.verifier_fiat_shamir();
        let weights = self.sample_weights(
            &fiat_shamir_hash,
            1 + 2 * boundary_quotient_mt_roots.len() + 2 * transition_constraints.len(),
        );

        // Verify low degree of combination polynomial, and collect indices
        // Note that FRI verifier verifies number of samples, so we don't have
        // to check that number here
        let indices_and_combination_values: Vec<(usize, PrimeFieldElementFlexible)> =
            self.fri.verify(proof_stream)?;

        let indices: Vec<usize> = indices_and_combination_values
            .iter()
            .map(|(i, _y)| *i)
            .collect();
        let combination_values: Vec<PrimeFieldElementFlexible> = indices_and_combination_values
            .iter()
            .map(|(_i, y)| *y)
            .collect();

        let mut duplicated_indices = indices.clone();
        duplicated_indices.append(
            &mut indices
                .iter()
                .map(|i| (*i + self.expansion_factor) % self.fri.domain.length)
                .collect(),
        );
        duplicated_indices.sort_unstable();
        println!("dup {:?}", duplicated_indices);

        // Read and verify boundary quotient leafs
        // revealed boundary quotient codeword values, indexed by (register, codeword index)
        let mut boundary_quotients: Vec<HashMap<usize, PrimeFieldElementFlexible>> = vec![];
        for (i, bq_root) in boundary_quotient_mt_roots.into_iter().enumerate() {
            boundary_quotients.push(HashMap::new());

            let auth_paths: Vec<PartialAuthenticationPath<StarkPfeDigest>> =
                proof_stream.dequeue_length_prepended()?;
            let bq_values: Vec<PrimeFieldElementFlexible> =
                proof_stream.dequeue_length_prepended()?;
            let leaves: Vec<_> = bq_values.iter().map(|x| hasher.hash(x)).collect();
            let auth_pairs: Vec<_> = zip(auth_paths, leaves).collect();

            let valid = MerkleTree::<StarkPfeHasher>::verify_multi_proof(
                bq_root,
                &duplicated_indices,
                &auth_pairs,
            );
            if !valid {
                return Err(Box::new(StarkVerifyError::BadMerkleProof(
                    MerkleProofError::BoundaryQuotientError(i),
                )));
            }

            duplicated_indices
                .iter()
                .zip(bq_values.iter())
                .for_each(|(index, value)| {
                    boundary_quotients[i].insert(*index, *value);
                });
        }

        // Read and verify randomizer leafs
        let randomizer_proof: Vec<PartialAuthenticationPath<StarkPfeDigest>> =
            proof_stream.dequeue_length_prepended()?;
        let randomizer_proof_values: Vec<PrimeFieldElementFlexible> =
            proof_stream.dequeue_length_prepended()?;

        let randomizer_proof_digests: Vec<_> = randomizer_proof_values
            .iter()
            .map(|x| hasher.hash(x))
            .collect();

        let auth_pairs: Vec<_> = zip(randomizer_proof, randomizer_proof_digests).collect();

        let valid_randomizer_auth_paths = MerkleTree::<StarkPfeHasher>::verify_multi_proof(
            randomizer_mt_root,
            &duplicated_indices,
            &auth_pairs,
        );
        if !valid_randomizer_auth_paths {
            return Err(Box::new(StarkVerifyError::BadMerkleProof(
                MerkleProofError::RandomizerError,
            )));
        }

        let mut randomizer_values: HashMap<usize, PrimeFieldElementFlexible> = HashMap::new();
        duplicated_indices
            .iter()
            .zip(randomizer_proof_values.iter())
            .for_each(|(index, value)| {
                randomizer_values.insert(*index, *value);
            });

        // Read and verify transition zerofier leafs
        let transition_zerofier_proof: Vec<PartialAuthenticationPath<StarkPfeDigest>> =
            proof_stream.dequeue_length_prepended()?;

        let transition_zerofier_proof_values: Vec<PrimeFieldElementFlexible> =
            proof_stream.dequeue_length_prepended()?;

        let transition_zerofier_proof_digests: Vec<_> = transition_zerofier_proof_values
            .iter()
            .map(|x| hasher.hash(x))
            .collect();
        let transition_zerofier_proof_auth_pairs: Vec<_> =
            zip(transition_zerofier_proof, transition_zerofier_proof_digests).collect();

        let valid = MerkleTree::<StarkPfeHasher>::verify_multi_proof(
            transition_zerofier_mt_root.to_owned(),
            &duplicated_indices,
            &transition_zerofier_proof_auth_pairs,
        );
        //assert!(false, "SADFDSAFDSADSAGSAGDSAF");
        if !valid {
            return Err(Box::new(StarkVerifyError::BadMerkleProof(
                MerkleProofError::TransitionZerofierError,
            )));
        }

        let mut transition_zerofier_values: HashMap<usize, PrimeFieldElementFlexible> =
            HashMap::new();
        duplicated_indices
            .iter()
            .zip(transition_zerofier_proof_values.iter())
            .for_each(|(index, value)| {
                transition_zerofier_values.insert(*index, *value);
            });

        // Verify leafs of combination polynomial
        let formatted_bcs = self.format_boundary_constraints(boundary_constraints);
        let boundary_zerofiers =
            get_boundary_zerofiers(formatted_bcs.clone(), self.omega.ring_one());
        let boundary_interpolants = get_boundary_interpolants(formatted_bcs);
        let max_degree = self.max_degree(&transition_constraints);
        let boundary_degrees = self.boundary_quotient_degree_bounds(&boundary_zerofiers);
        let expected_tq_degrees = self.transition_quotient_degree_bounds(&transition_constraints);
        for (i, current_index) in indices.into_iter().enumerate() {
            let current_x: PrimeFieldElementFlexible =
                self.field_generator * self.omega.mod_pow(current_index.into());
            let next_index: usize =
                (current_index + self.expansion_factor) % self.fri.domain.length;
            let next_x: PrimeFieldElementFlexible =
                self.field_generator * self.omega.mod_pow(next_index.into());
            let mut current_trace: Vec<PrimeFieldElementFlexible> = (0..self.register_count)
                .map(|r| {
                    boundary_quotients[r][&current_index]
                        * boundary_zerofiers[r].evaluate(&current_x)
                        + boundary_interpolants[r].evaluate(&current_x)
                })
                .collect();
            let mut next_trace: Vec<PrimeFieldElementFlexible> = (0..self.register_count)
                .map(|r| {
                    boundary_quotients[r][&next_index] * boundary_zerofiers[r].evaluate(&next_x)
                        + boundary_interpolants[r].evaluate(&next_x)
                })
                .collect();

            let mut point: Vec<PrimeFieldElementFlexible> = vec![current_x];
            point.append(&mut current_trace);
            point.append(&mut next_trace);

            let transition_constraint_values: Vec<PrimeFieldElementFlexible> =
                transition_constraints
                    .iter()
                    .map(|tc| tc.evaluate(&point))
                    .collect();

            // Get combination polynomial evaluation value
            // Loop over all registers for transition quotient values, and for boundary quotient values
            let mut terms: Vec<PrimeFieldElementFlexible> = vec![randomizer_values[&current_index]];
            for (tcv, tq_degree) in transition_constraint_values
                .iter()
                .zip(expected_tq_degrees.iter())
            {
                let transition_quotient =
                    tcv.to_owned() / transition_zerofier_values[&current_index];
                terms.push(transition_quotient);
                let shift = max_degree - tq_degree;
                terms.push(transition_quotient * current_x.mod_pow(shift.into()));
            }
            for (bqvs, bq_degree) in boundary_quotients.iter().zip(boundary_degrees.iter()) {
                terms.push(bqvs[&current_index]);
                let shift = max_degree - bq_degree;
                terms.push(bqvs[&current_index] * current_x.mod_pow(shift.into()));
            }

            assert_eq!(
                weights.len(),
                terms.len(),
                "weights and terms length must match in verifier"
            );
            let combination = weights
                .iter()
                .zip(terms.iter())
                .fold(self.omega.ring_zero(), |sum, (weight, term)| {
                    sum + term.to_owned() * weight.to_owned()
                });

            if combination_values[i] != combination {
                return Err(Box::new(StarkVerifyError::LinearCombinationMismatch(
                    current_index,
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
pub mod test_stark_pfef {
    use primitive_types::U256;

    use crate::shared_math::rescue_prime_pfe_flexible::RescuePrime;

    use super::*;

    pub fn get_tutorial_stark() -> (StarkPrimeFieldElementFlexible, RescuePrime) {
        let expansion_factor = 4;
        let colinearity_checks_count = 2;
        let rescue_prime = RescuePrime::from_tutorial();
        let register_count = rescue_prime.m;
        let cycles_count = rescue_prime.steps_count + 1;
        let transition_constraints_degree = 2;
        let prime: U256 = (407u128 * (1 << 119) + 1).into();
        let generator = PrimeFieldElementFlexible::new(
            85408008396924667383611388730472331217u128.into(),
            prime,
        );

        (
            StarkPrimeFieldElementFlexible::new(
                expansion_factor,
                colinearity_checks_count,
                register_count,
                cycles_count,
                transition_constraints_degree,
                generator,
            ),
            rescue_prime,
        )
    }

    #[test]
    fn ready_for_verify_and_prove_test() {
        let (mut stark, _) = get_tutorial_stark();
        assert!(!stark.ready_for_verify());
        assert!(!stark.ready_for_prove());
        stark.set_transition_zerofier_mt_root(0u128.into());
        assert!(stark.ready_for_verify());
        assert!(!stark.ready_for_prove());
        stark.prover_preprocess();
        assert!(stark.ready_for_verify());
        assert!(stark.ready_for_prove());
    }

    #[test]
    fn prng_with_seed() {
        let mut rng = Pcg64::seed_from_u64(2);
        let mut rand_bytes = [0u8; 32];
        rng.fill_bytes(&mut rand_bytes);

        let prime: U256 = (407u128 * (1 << 119) + 1).into();
        let one = PrimeFieldElementFlexible::new(1.into(), prime);
        let fe = one.from_vecu8(rand_bytes.to_vec());
        println!("fe = {}", fe);
        let expected = PrimeFieldElementFlexible::new(
            114876749706552506467803119432194128310u128.into(),
            prime,
        );
        assert_eq!(expected, fe);
    }

    #[test]
    fn boundary_quotient_degree_bounds_test() {
        let prime: U256 = (407u128 * (1 << 119) + 1).into();
        let (stark, rescue_prime) = get_tutorial_stark();
        let input = PrimeFieldElementFlexible::new(228894434762048332457318u128.into(), prime);
        let output_element = rescue_prime.hash(&input);
        let boundary_constraints = rescue_prime.get_boundary_constraints(output_element);
        let bcs_formatted = stark.format_boundary_constraints(boundary_constraints);
        let boundary_zerofiers: Vec<Polynomial<PrimeFieldElementFlexible>> =
            get_boundary_zerofiers(bcs_formatted, stark.omega.ring_one());
        let degrees = stark.boundary_quotient_degree_bounds(&boundary_zerofiers);
        assert_eq!(vec![34, 34], degrees);
    }

    #[test]
    fn max_degree_test() {
        let (stark, rescue_prime) = get_tutorial_stark();
        let res = stark.max_degree(&rescue_prime.get_air_constraints(stark.omicron));
        assert_eq!(127usize, res);
    }

    #[test]
    fn transition_quotient_degree_bounds_test() {
        let (stark, rescue_prime) = get_tutorial_stark();
        let res = stark
            .transition_quotient_degree_bounds(&rescue_prime.get_air_constraints(stark.omicron));
        // tq.degree()
        // = ((rp.step_count + num_randomizer )* air_constraints.degree()) - transition_zerofier.degree()
        // = (27 + 8) * 3 - 27 = 78
        assert_eq!(vec![78, 78], res);
    }

    #[test]
    fn transition_degree_bounds_test() {
        let (stark, rescue_prime) = get_tutorial_stark();
        let res = stark.transition_degree_bounds(&rescue_prime.get_air_constraints(stark.omicron));
        assert_eq!(vec![105, 105], res);
    }

    #[test]
    fn rescue_prime_stark() {
        let prime: U256 = (407u128 * (1 << 119) + 1).into();
        let (mut stark, rescue_prime) = get_tutorial_stark();
        stark.prover_preprocess(); // Prepare STARK for proving

        let input = PrimeFieldElementFlexible::new(228894434762048332457318u128.into(), prime);
        let trace = rescue_prime.trace(&input);
        let output_element = trace[rescue_prime.steps_count][0];
        let transition_constraints = rescue_prime.get_air_constraints(stark.omicron);
        let boundary_constraints = rescue_prime.get_boundary_constraints(output_element);
        let mut proof_stream = ProofStream::default();

        let stark_proof = stark.prove(
            trace,
            transition_constraints.clone(),
            boundary_constraints.clone(),
            &mut proof_stream,
        );
        match stark_proof {
            Ok(()) => (),
            Err(_) => panic!("Failed to produce STARK proof."),
        }
        let verify = stark.verify(
            &mut proof_stream,
            transition_constraints,
            boundary_constraints,
        );
        match verify {
            Ok(_) => (),
            Err(err) => panic!("Verification of STARK proof failed with error: {}", err),
        };
    }

    #[test]
    fn rp_stark_test() {
        let prime: U256 = (407u128 * (1 << 119) + 1).into();
        let expansion_factor = 4usize;
        let colinearity_checks_count = 2usize;
        let transition_constraints_degree = 2usize;
        let generator = PrimeFieldElementFlexible::new(
            85408008396924667383611388730472331217u128.into(),
            prime,
        );
        let rescue_prime_stark = RescuePrime::from_tutorial();

        let mut stark = StarkPrimeFieldElementFlexible::new(
            expansion_factor,
            colinearity_checks_count,
            rescue_prime_stark.m,
            rescue_prime_stark.steps_count + 1,
            transition_constraints_degree,
            generator,
        );
        stark.prover_preprocess();

        let one = PrimeFieldElementFlexible::new(1.into(), prime);
        let trace = rescue_prime_stark.trace(&one);
        let air_constraints = rescue_prime_stark.get_air_constraints(stark.omicron);
        let hash_result = trace.last().unwrap()[0];
        let boundary_constraints: Vec<BoundaryConstraint> =
            rescue_prime_stark.get_boundary_constraints(hash_result);
        let mut proof_stream = ProofStream::default();
        let _proof = stark.prove(
            trace,
            air_constraints,
            boundary_constraints,
            &mut proof_stream,
        );
    }
}
